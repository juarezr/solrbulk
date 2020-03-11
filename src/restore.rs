use crossbeam::channel::{Receiver, Sender};
use crossbeam::crossbeam_channel::bounded;
use crossbeam::thread;
use log::{debug, error, info};

use std::path::PathBuf;
use std::time::Instant;

use crate::args::Restore;
use crate::bars::*;
use crate::fails::*;
use crate::helpers::*;
use crate::ingest::*;

pub(crate) fn restore_main(params: Restore) -> Result<(), BoxedError> {
    debug!("  {:?}", params);

    let found = params.find_archives()?;

    if found.is_empty() {
        throw(format!("Found no archives to restore from: {}\n note: try to specify the option --pattern with the source core name", 
            params.get_pattern()))?;
    }

    let core = params.into.clone();
    info!(
        "Indexing documents in solr core {} from: {:?}",
        core, params.from
    );

    let started = Instant::now();

    unzip_archives(params, found)?;

    info!("Updated solr core {} in {:?}.", core, started.elapsed());
    Ok(())
}

fn unzip_archives(params: Restore, found: Vec<PathBuf>) -> Result<(), BoxedError> {
    let doc_count = estimate_document_count(&found)?;

    thread::scope(|pool| {
        let readers_channel = params.readers * 2;
        let writers_channel = params.writers * 2;

        let (generator, sequence) = bounded::<&PathBuf>(readers_channel);
        let (sender, receiver) = bounded::<String>(writers_channel);
        let (progress, reporter) = bounded::<u64>(params.writers);

        let archives = found.iter();
        pool.spawn(|_| {
            for archive in archives {
                generator.send(archive).unwrap();
            }
            drop(generator);
        });

        for ir in 0..params.readers {
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

        for iw in 0..params.writers {
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
        }
        perc_bar.finish_and_clear();
        drop(reporter);
    })
    .unwrap();

    Ok(())
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

fn start_reading_archive(reader: usize, iterator: Receiver<&PathBuf>, producer: Sender<String>) {
    debug!("  Reading #{}", reader);

    loop {
        let received = iterator.recv();
        if let Ok(archive_path) = received {
            let can_open = ArchiveReader::create_reader(&archive_path);
            match can_open {
                Err(cause) => {
                    error!("Error reading documents in archive: {}", cause);
                    break;
                }
                Ok(archive_reader) => {
                    for docs in archive_reader {
                        producer.send(docs).unwrap();
                    }
                }
            }
        } else {
            break;
        }
    }
    drop(producer);

    debug!("  Finished Reading #{}", reader);
}

fn start_indexing_docs(
    writer: usize,
    url: &str,
    consumer: Receiver<String>,
    progress: Sender<u64>,
) {
    debug!("  Storing #{}", writer);

    loop {
        let received = consumer.recv();
        if let Ok(docs) = received {
            let failed = post_content(url, docs);
            if let Err(cause) = failed {
                error!("Error writing file into archive: {}", cause);
                break;
            }
            progress.send(0).unwrap();
        } else {
            break;
        }
    }
    drop(consumer);

    debug!("  Finished Storing #{}", writer);
}

// endregion

#[cfg(test)]
mod tests {
    use crate::args::*;
    use crate::fails::*;

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
