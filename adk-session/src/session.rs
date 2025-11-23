use crate::{Events, State};
use chrono::{DateTime, Utc};

pub trait Session: Send + Sync {
    fn id(&self) -> &str;
    fn app_name(&self) -> &str;
    fn user_id(&self) -> &str;
    fn state(&self) -> &dyn State;
    fn events(&self) -> &dyn Events;
    fn last_update_time(&self) -> DateTime<Utc>;
}

pub const KEY_PREFIX_APP: &str = "app:";
pub const KEY_PREFIX_TEMP: &str = "temp:";
pub const KEY_PREFIX_USER: &str = "user:";
