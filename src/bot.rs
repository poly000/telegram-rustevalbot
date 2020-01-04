use derive_more::From;
use futures::future::TryFutureExt as _;
use futures::Stream;
use log::debug;
use reqwest;
use reqwest::{Client, Request};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use telegram_types::bot::inline_mode::{AnswerInlineQuery, InlineQueryId, InlineQueryResult};
use telegram_types::bot::methods::{
    ApiError, ChatTarget, DeleteMessage, EditMessageText, GetMe, GetUpdates, Method, SendMessage,
    TelegramResult,
};
use telegram_types::bot::types::{ChatId, Message, MessageId, ParseMode, Update, UpdateId};
use tokio::time::{timeout, Elapsed};

/// Telegram bot
#[derive(Clone, Debug)]
pub struct Bot {
    client: Client,
    token: &'static str,
    /// Telegram username of the bot
    pub username: &'static str,
}

impl Bot {
    pub async fn create(client: Client, token: &'static str) -> Result<Self, Error> {
        let bot = Bot {
            client,
            token,
            username: "",
        };
        let user = bot.build_request(&GetMe).execute().await?;
        let username = Box::leak(user.username.expect("No username?").into_boxed_str());
        Ok(Bot { username, ..bot })
    }

    pub fn with_client(self, client: Client) -> Self {
        Bot { client, ..self }
    }

    pub fn get_updates(&self) -> UpdateStream {
        UpdateStream {
            bot: self.clone(),
            update_id: None,
            buffer: VecDeque::new(),
            current_request: None,
        }
    }

    pub fn confirm_update(&self, update_id: UpdateId) -> impl Future<Output = Result<(), Error>> {
        let mut get_updates = GetUpdates::new();
        get_updates.offset(UpdateId(update_id.0 + 1));
        self.build_request(&get_updates).execute().map_ok(|_| ())
    }

    pub fn send_message<'a>(
        &self,
        chat_id: ChatId,
        text: impl Into<Cow<'a, str>>,
    ) -> BotRequest<Message> {
        let mut send_message =
            SendMessage::new(ChatTarget::id(chat_id.0), text).parse_mode(ParseMode::HTML);
        send_message.disable_web_page_preview = Some(true);
        self.build_request(&send_message)
    }

    pub fn edit_message<'a>(
        &self,
        chat_id: ChatId,
        message_id: MessageId,
        text: impl Into<Cow<'a, str>>,
    ) -> BotRequest<Message> {
        let edit_message = EditMessageText::new(ChatTarget::id(chat_id.0), message_id, text)
            .parse_mode(ParseMode::HTML)
            .disable_preview();
        self.build_request(&edit_message)
    }

    pub fn delete_message(&self, chat_id: ChatId, message_id: MessageId) -> BotRequest<bool> {
        let delete_message = DeleteMessage {
            chat_id: ChatTarget::id(chat_id.0),
            message_id,
        };
        self.build_request(&delete_message)
    }

    pub fn answer_inline_query(
        &self,
        inline_query_id: InlineQueryId,
        results: &[InlineQueryResult<'_>],
    ) -> BotRequest<bool> {
        let answer = AnswerInlineQuery {
            inline_query_id,
            results: results.into(),
            cache_time: None,
            is_personal: None,
            next_offset: None,
            switch_pm_text: None,
            switch_pm_parameter: None,
        };
        self.build_request(&answer)
    }

    fn build_request<R>(&self, request: &R) -> BotRequest<R::Item>
    where
        R: Method + Serialize,
    {
        let request = self.client.post(&R::url(self.token)).json(&request).build();
        BotRequest {
            client: self.client.clone(),
            request,
            phantom: PhantomData,
        }
    }
}

pub struct BotRequest<T> {
    client: Client,
    request: Result<Request, reqwest::Error>,
    phantom: PhantomData<T>,
}

impl<T> BotRequest<T>
where
    T: Send,
    for<'de> T: Deserialize<'de>,
{
    pub async fn execute(self) -> Result<T, Error> {
        let req = self.request?;
        let resp = self.client.execute(req).await?;
        let data = resp.bytes().await?;
        match serde_json::from_slice::<TelegramResult<T>>(&data) {
            Ok(result) => Into::<Result<_, _>>::into(result).map_err(Error::Api),
            Err(error) => Err(Error::Parse(ParseError {
                data: data.into_iter().collect(),
                error,
            })),
        }
    }
}

#[derive(Debug, From)]
pub enum Error {
    Request(reqwest::Error),
    Api(ApiError),
    Parse(ParseError),
}

pub struct ParseError {
    pub data: Vec<u8>,
    pub error: serde_json::Error,
}

impl fmt::Debug for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "ParseError: {}", self.error)
    }
}

pub struct UpdateStream {
    bot: Bot,
    update_id: Option<UpdateId>,
    buffer: VecDeque<Update>,
    current_request: Option<PendingFuture>,
}

impl UpdateStream {
    pub fn bot(&self) -> &Bot {
        &self.bot
    }
}

type PendingFuture =
    Pin<Box<dyn Future<Output = Result<Result<Vec<Update>, Error>, Elapsed>> + Send>>;

const TELEGRAM_TIMEOUT_SECS: u16 = 5;

impl Stream for UpdateStream {
    type Item = Result<Update, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let mut_self = self.get_mut();
        loop {
            if let Some(update) = mut_self.buffer.pop_front() {
                debug!("{}: {:?}", mut_self.bot.username, update);
                break Poll::Ready(Some(Ok(update)));
            }
            let mut request = mut_self.current_request.take().unwrap_or_else(|| {
                let mut get_updates = GetUpdates::new();
                if let Some(update_id) = mut_self.update_id {
                    get_updates.offset(update_id);
                }
                get_updates.timeout = Some(i32::from(TELEGRAM_TIMEOUT_SECS));
                Box::pin(timeout(
                    Duration::from_secs(u64::from(TELEGRAM_TIMEOUT_SECS)),
                    mut_self.bot.build_request(&get_updates).execute(),
                ))
            });
            match Pin::new(&mut request).poll(cx) {
                Poll::Ready(Ok(Ok(updates))) => {
                    if let Some(last_update) = updates.last() {
                        mut_self.bump_update_id(last_update.update_id);
                    }
                    mut_self.buffer = VecDeque::from(updates);
                }
                Poll::Pending => {
                    mut_self.current_request = Some(request);
                    break Poll::Pending;
                }
                Poll::Ready(Ok(Err(err))) => {
                    mut_self.may_recover_from_error(&err);
                    break Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(Err(_elapsed)) => {
                    // Timeout, loop back and do a new one.
                }
            }
        }
    }
}

impl UpdateStream {
    fn bump_update_id(&mut self, update_id: UpdateId) {
        self.update_id = Some(UpdateId(update_id.0 + 1));
    }

    fn may_recover_from_error(&mut self, error: &Error) {
        // XXX We should be able to simplify this function once if-let-chain
        // gets stable. See RFC 2497.
        let data = match error {
            Error::Parse(ParseError { data, .. }) => data,
            _ => return,
        };
        let value = match serde_json::from_slice::<JsonValue>(&data) {
            Ok(value) => value,
            Err(_) => return,
        };
        let map = match value {
            JsonValue::Object(map) => map,
            _ => return,
        };
        let ok = map.get("ok").and_then(|v| v.as_bool());
        if !ok.unwrap_or(false) {
            return;
        }
        let update_id = map
            .get("result")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.last())
            .and_then(|item| item.as_object())
            .and_then(|map| map.get("update_id"))
            .and_then(|v| v.as_i64());
        if let Some(update_id) = update_id {
            self.bump_update_id(UpdateId(update_id));
        }
    }
}
