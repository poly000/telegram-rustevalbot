mod crate_;
mod eval;
mod meta;

use futures::{Future, IntoFuture};
use futures::unsync::oneshot;
use reqwest::unstable::async::Client;
use std::borrow::Cow;
use std::fmt::Display;
use utils::is_separator;

/// Command executor.
pub struct Executor<'a> {
    /// Reqwest client
    pub client: &'a Client,
    /// A oneshot sender for shuting down the event loop
    pub shutdown: Option<oneshot::Sender<()>>,
}

impl<'a> Executor<'a> {
    /// Execute a command.
    ///
    /// Future resolves to a message to send back. If nothing can be
    /// replied, it rejects.
    pub fn execute(
        &mut self,
        command: &str,
        is_admin: bool,
    ) -> Box<Future<Item=Cow<'static, str>, Error=()>> {
        fn reply(
            reply: Result<
                impl Into<Cow<'static, str>>,
                impl Display
            >
        ) -> Result<Cow<'static, str>, ()> {
            Ok(match reply {
                Ok(reply) => reply.into(),
                Err(err) => format!("error: {}", err).into(),
            })
        }
        match split_command(command) {
            ("/crate", param) =>
                Box::new(crate_::run(self.client, param).then(reply)),
            ("/eval", param) =>
                Box::new(eval::run(self.client, param).then(reply)),
            ("/meta", param) =>
                Box::new(meta::run(self.client, param).then(reply)),
            ("/version", "") => Box::new(version()),
            ("/shutdown", "") if is_admin => Box::new(self.shutdown()),
            _ => Box::new(Err(()).into_future()),
        }
    }

    fn shutdown(&mut self) -> impl Future<Item=Cow<'static, str>, Error=()> {
        self.shutdown.take().unwrap().send(()).unwrap();
        Ok("bye".into()).into_future()
    }
}

fn split_command<'a>(s: &'a str) -> (&'a str, &'a str) {
    match s.find(is_separator) {
        Some(pos) => (&s[..pos], &s[pos + 1..]),
        None => (s, ""),
    }
}

fn version() -> impl Future<Item=Cow<'static, str>, Error=()> {
    Ok(format!("version: {}", super::VERSION).into()).into_future()
}
