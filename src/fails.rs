#![allow(dead_code)]

use std::error::Error;
use std::fmt;
    
pub type BoxedError = Box<dyn Error>;
    
pub type BoxedResult<T> = Result<T, BoxedError>;
    
pub type ErrorResult = Result<(), BoxedError>;

pub struct Failed {
    details: String
}

impl Failed {
    pub fn new(msg: &str) -> Self {
        Self { details: msg.to_string() }
    }

    fn say(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f,"{}", self.details)
    }
}

pub fn throw<T>(message: String) -> Result<T, BoxedError> {
    Err(Box::new(Failed::new(&message)))
}

pub fn raise<T>(message: &str) -> Result<T, BoxedError> {
    Err(Box::new(Failed::new(&message)))
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.say(f)
    }
}

impl fmt::Debug for Failed {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.say(f)
    }
}

impl Error for Failed {
    fn description(&self) -> &str {
        &self.details
    }
}

#[cfg(test)]
mod test {
    use crate::fails::*;
   
    #[test]
    fn check_throw() {

        assert_eq!(throw::<usize>("fail".to_string()).is_ok(), false );
        assert_eq!(raise::<usize>("fail").is_ok(), false );
    }
}
