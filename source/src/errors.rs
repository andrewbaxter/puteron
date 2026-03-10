use {
    loga::Log,
};

pub trait ErrorHandler: Send {
    fn handle(&mut self, e: loga::Error);
}

pub struct ReturnErrorHandler {
    pub errors: Vec<loga::Error>,
}

impl ErrorHandler for ReturnErrorHandler {
    fn handle(&mut self, e: loga::Error) {
        self.errors.push(e);
    }
}

pub struct LogErrorHandler {
    pub log: Log,
}

impl ErrorHandler for LogErrorHandler {
    fn handle(&mut self, e: loga::Error) {
        self.log.log_err(loga::WARN, e);
    }
}
