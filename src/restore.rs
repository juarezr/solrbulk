use crossbeam_channel::{bounded, Receiver, Sender};
use crossbeam_utils::thread;
use log::{debug, error, info};

use std::sync::{atomic::AtomicBool, Arc};
use std::{path::PathBuf, time::Instant};

use crate::{
    args::Restore, bars::*, connection::SolrClient, fails::*, helpers::*, ingest::*, state::*,
};

pub(crate) fn restore_main(params: Restore) -> Result<(), BoxedError> {
    debug!("  {:?}", params);

    let found = params.find_archives()?;

    if found.is_empty() {
        throw(format!(
            "Found no archives to restore from: {}\n note: try to specify the option --pattern \
             with the source core name",
            params.get_pattern()
        ))?;
    }

    let core = params.into.clone();
    info!("Indexing documents in solr core {} from: {:?}", core, params.from);

    let started = Instant::now();

    let updated = unzip_archives(params, &found)?;

    info!("Updated {} batches in solr core {} in {:?}.", updated, core, started.elapsed());
    Ok(())
}

fn unzip_archives(params: Restore, found: &[PathBuf]) -> Result<usize, BoxedError> {
    let doc_count = estimate_document_count(found)?;
    let mut updated = 0;

    thread::scope(|pool| {
        let transfer = &params.transfer;
        let readers_channel = transfer.readers * 2;
        let writers_channel = transfer.writers * 2;

        let (generator, sequence) = bounded::<&PathBuf>(readers_channel);
        let (sender, receiver) = bounded::<String>(writers_channel);
        let (progress, reporter) = bounded::<u64>(transfer.writers);

        pool.spawn(move |_| {
            start_listing_archives(found, generator);
        });

        for ir in 0..transfer.readers {
            let producer = sender.clone();
            let iterator = sequence.clone();

            let reader = ir;
            let thread_name = format!("Reader_{}", reader);
            pool.builder()
                .name(thread_name)
                .spawn(move |_| {
                    start_reading_archive(reader, iterator, producer);
                })
                .unwrap();
        }
        drop(sequence);
        drop(sender);

        let update_hadler_url = params.get_update_url();

        for iw in 0..transfer.writers {
            let consumer = receiver.clone();
            let updater = progress.clone();

            let url = update_hadler_url.clone();

            let writer = iw;
            let thread_name = format!("Writer_{}", writer);
            pool.builder()
                .name(thread_name)
                .spawn(move |_| {
                    start_indexing_docs(writer, &url, consumer, updater);
                })
                .unwrap();
        }
        drop(receiver);
        drop(progress);

        let perc_bar = new_wide_bar(doc_count);
        for _ in reporter.iter() {
            perc_bar.inc(1);
            updated += 1;
        }
        perc_bar.finish_and_clear();
        drop(reporter);
    })
    .unwrap();

    Ok(updated)
}

fn estimate_document_count(found: &[PathBuf]) -> Result<u64, BoxedError> {
    // Estimate number of json files inside all zip files
    let zip_count = found.len();

    let first = found.first().unwrap();
    let file_count = ArchiveReader::get_archive_file_count(first);
    match file_count {
        None => throw(format!("Error opening archive: {:?}", first))?,
        Some(doc_count) => {
            let doc_total = doc_count * zip_count;
            Ok(doc_total.to_u64())
        }
    }
}

// region Channels

fn start_listing_archives<'a>(found: &'a [PathBuf], generator: Sender<&'a PathBuf>) {
    let archives = found.iter();
    for archive in archives {
        let status = generator.send(&archive);
        if status.is_err() {
            break;
        }
    }
    drop(generator);
}

fn start_reading_archive(reader: usize, iterator: Receiver<&PathBuf>, producer: Sender<String>) {
    let ctrl_c = monitor_term_sinal();

    loop {
        let received = iterator.recv();
        let failed = match received {
            Ok(archive_path) => handle_reading_archive(reader, &producer, archive_path, &ctrl_c),
            Err(_) => true,
        };
        if failed || ctrl_c.aborted() {
            break;
        }
    }
    drop(producer);
}

fn handle_reading_archive(
    reader: usize, producer: &Sender<String>, archive_path: &PathBuf, ctrl_c: &Arc<AtomicBool>,
) -> bool {
    let can_open = ArchiveReader::create_reader(&archive_path);
    match can_open {
        Err(cause) => {
            error!("Error in thread #{} while reading docs in zip: {}", reader, cause);
            true
        }
        Ok(archive_reader) => {
            for docs in archive_reader {
                let status = producer.send(docs);
                if status.is_err() || ctrl_c.aborted() {
                    return true;
                }
            }
            false
        }
    }
}

fn start_indexing_docs(
    writer: usize, url: &str, consumer: Receiver<String>, progress: Sender<u64>,
) {
    let ctrl_c = monitor_term_sinal();

    let mut client = SolrClient::new().unwrap();
    loop {
        let received = consumer.recv();
        if ctrl_c.aborted() {
            break;
        }
        let failed = match received {
            Ok(docs) => handle_received_batch(docs, writer, url, &mut client, &progress),
            Err(_) => true,
        };
        if failed || ctrl_c.aborted() {
            break;
        }
    }
    drop(consumer);
}

fn handle_received_batch(
    docs: String, writer: usize, url: &str, client: &mut SolrClient, progress: &Sender<u64>,
) -> bool {
    let failed = client.post_as_json(&url, docs);
    wait(1);
    if let Err(cause) = failed {
        error!("Error in thread # {} while sending docs to solr core: {:?}", writer, cause);
        true
    } else {
        let status = progress.send(0);
        status.is_err()
    }
}

// endregion

#[cfg(test)]
mod tests {
    use crate::{args::*, fails::*};

    impl Arguments {
        pub fn put(&self) -> Result<&Restore, BoxedError> {
            match &self {
                Self::Restore(puts) => Ok(&puts),
                _ => raise("command must be 'restore' !"),
            }
        }
    }

    #[test]
    fn check_restore_pattern() {
        let parsed = Arguments::mockup_args_restore();
        let puts = parsed.put().unwrap();
        let wilcard = puts.get_pattern();
        assert_eq!(wilcard.ends_with(".zip"), true);
    }

    #[test]
    fn check_restore_iterator() {
        let parsed = Arguments::mockup_args_restore();
        let puts = parsed.put().unwrap();

        for zip in puts.find_archives().unwrap() {
            println!("{:?}", zip);
            let path = zip.to_str().unwrap();
            assert_eq!(path.ends_with(".zip"), true);
        }
    }
}
