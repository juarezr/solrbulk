use log::{debug, trace};
use std::{error::Error, fmt};

use crate::helpers::*;

// region SolrError

#[derive(Debug)]
pub struct SolrError {
    pub details: String,
}

impl SolrError {
    pub fn new(message: String, body: String) -> Self {
        let msg = if body.is_empty() {
            format!("Solr Error: {}", message)
        } else {
            format!("Solr Error: {} -> Reponse: {}", message, body)
        };
        SolrError { details: msg }
    }

    fn say(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.details)
    }
}

impl fmt::Display for SolrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.say(f)
    }
}

impl Error for SolrError {
    fn description(&self) -> &str {
        &self.details
    }
}

// endregion

// region SolrClient

#[derive(Debug)]
pub struct SolrClient {
    http: ureq::Agent,
    max_retries: usize,
    retry_count: usize,
}

// TODO: authentication, proxy, etc...

const SOLR_COPY_TIMEOUT: &str = "SOLR_COPY_TIMEOUT";
const SOLR_COPY_RETRIES: &str = "SOLR_COPY_RETRIES";

const SOLR_DEF_TIMEOUT: isize = 60;
const SOLR_DEF_RETRIES: isize = 8;

impl SolrClient {
    pub fn new() -> Self {
        let retries = env_value(SOLR_COPY_RETRIES, SOLR_DEF_RETRIES);
        let client = ureq::agent()
            // .basic_auth("admin", Some("good password"))
            .build();

        SolrClient { http: client, max_retries: retries.to_usize(), retry_count: 0 }
    }

    fn get_timeout() -> u64 {
        let def = if cfg!(debug_assertions) { 6 } else { SOLR_DEF_TIMEOUT };
        let timeout: isize = env_value(SOLR_COPY_TIMEOUT, def);
        timeout.to_u64() * 1000
    }

    fn set_timeout(builder: &mut ureq::Request) -> &mut ureq::Request {
        let timeout = Self::get_timeout();
        builder
            .timeout_connect(timeout)
            .timeout_read(Self::get_timeout())
            .timeout_write(Self::get_timeout())
    }

    pub fn get_as_text(&mut self, url: &str) -> Result<String, SolrError> {
        let mut builder = self.http.get(url);
        let request = Self::set_timeout(&mut builder);
        loop {
            let response = request.call();
            let result = self.handle_response(response);
            match result {
                None => continue,
                Some(retrieved) => break retrieved,
            }
        }
    }

    pub fn post_as_json(&mut self, url: &str, content: &str) -> Result<String, SolrError> {
        self.post_with_content_type(url, "application/json", content)
    }

    pub fn post_as_xml(&mut self, url: &str, content: &str) -> Result<String, SolrError> {
        self.post_with_content_type(url, "application/xml", content)
    }

    fn post_with_content_type(
        &mut self, url: &str, content_type: &str, content: &str,
    ) -> Result<String, SolrError> {
        let mut builder = self.http.post(url);
        let req = Self::set_timeout(&mut builder);
        let request = req.set("Content-Type", content_type);
        loop {
            let response = request.send_string(content);
            let result = self.handle_response(response);
            match result {
                None => continue,
                Some(retrieved) => break retrieved,
            }
        }
    }

    fn handle_response(&mut self, response: ureq::Response) -> Option<Result<String, SolrError>> {
        let result = self.get_result_from(response);
        match result {
            Ok(retrieved) => {
                if self.retry_count > 0 {
                    self.retry_count -= 1;
                }
                Some(Ok(retrieved))
            }
            Err(failure) => {
                match self.handle_failure(failure) {
                    None => {
                        self.retry_count += 1;
                        // wait a little for the server recovering before retrying
                        wait(5 * self.retry_count);
                        None
                    }
                    Some(failed) => Some(Err(failed)),
                }
            }
        }
    }

    fn get_result_from(
        &mut self, response: ureq::Response,
    ) -> Result<String, Result<ureq::Response, std::io::Error>> {
        if response.error() {
            Err(Ok(response))
        } else {
            match response.into_string() {
                Ok(body) => Ok(body),
                Err(read_error) => Err(Err(read_error)),
            }
        }
    }

    fn handle_failure(
        &mut self, failure: Result<ureq::Response, std::io::Error>,
    ) -> Option<SolrError> {
        let can_retry = self.retry_count < self.max_retries;
        match failure {
            Ok(response) => {
                if response.synthetic() {
                    Self::handle_synthetic_error(can_retry, response)
                } else {
                    Self::handle_solr_error(can_retry, response)
                }
            }
            Err(read_error) => Self::handle_receive_error(can_retry, read_error),
        }
    }

    fn handle_synthetic_error(can_retry: bool, response: ureq::Response) -> Option<SolrError> {
        let cause = response.synthetic_error().as_ref().unwrap();
        match cause {
            ureq::Error::ConnectionFailed(_) => {
                Self::convert_synthetic_error(can_retry, cause, &response)
            }
            ureq::Error::Io(failure) => {
                let error_kind = failure.kind();
                match error_kind {
                    std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::Interrupted => {
                        Self::convert_synthetic_error(can_retry, cause, &response)
                    }
                    _ => Self::convert_synthetic_error(can_retry, cause, &response),
                }
            }
            _ => Self::convert_synthetic_error(false, cause, &response),
        }
    }

    fn convert_synthetic_error(
        can_retry: bool, cause: &ureq::Error, response: &ureq::Response,
    ) -> Option<SolrError> {
        if can_retry {
            debug!(
                "Generic Error: Retry: {}, Status: {}",
                cause.to_string(),
                response.status_line()
            );
            return None;
        }
        let message = format!("Generic Error: {}", cause.status_text());
        let body = cause.body_text();
        trace!("Continue: {} -> {}", message, body);
        Some(SolrError::new(message, body))
    }

    fn handle_solr_error(can_retry: bool, response: ureq::Response) -> Option<SolrError> {
        let message = format!("Response Error: {}", response.status_line());
        // Retry on status 502 Bad Gateway
        // Retry on status 503 Service Temporarily Unavailable
        // Retry on status 504 Gateway Timeout
        if can_retry && response.server_error() {
            debug!("Retry: {}", message);
            return None;
        }
        let body = match response.into_string() {
            Ok(content) => content,
            Err(unread) => unread.to_string(),
        };
        trace!("Continue: {} -> {}", message, body);
        Some(SolrError::new(message, body))
    }

    fn handle_receive_error(can_retry: bool, error: std::io::Error) -> Option<SolrError> {
        let message = format!("Receive Error: {}", error.to_string());
        let body = format!("{:?}", error);
        if can_retry {
            debug!("Retry: {} -> {}", message, body);
            return None;
        }
        trace!("Continue: {} -> {}", message, body);
        Some(SolrError::new(message, body))
    }

    // region Helpers

    pub fn query_get_as_text(url: &str) -> Result<String, SolrError> {
        let mut con = SolrClient::new();
        con.get_as_text(url)
    }

    pub fn send_post_as_json(url: &str, content: &str) -> Result<String, SolrError> {
        let mut con = SolrClient::new();
        con.post_as_json(url, content)
    }

    pub fn send_post_as_xml(url: &str, content: &str) -> Result<String, SolrError> {
        let mut con = SolrClient::new();
        con.post_as_xml(url, content)
    }

    // endregion
}

// endregion
