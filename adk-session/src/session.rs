use crate::{Events, State};
use adk_core::types::{SessionId, UserId};
use chrono::{DateTime, Utc};

pub trait Session: Send + Sync {
    fn id(&self) -> &SessionId;
    fn app_name(&self) -> &str;
    fn user_id(&self) -> &UserId;
    fn state(&self) -> &dyn State;
    fn events(&self) -> &dyn Events;
    fn last_update_time(&self) -> DateTime<Utc>;
}

pub const KEY_PREFIX_APP: &str = "app:";
pub const KEY_PREFIX_TEMP: &str = "temp:";
pub const KEY_PREFIX_USER: &str = "user:";
