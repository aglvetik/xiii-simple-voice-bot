use std::{collections::HashMap, sync::atomic::AtomicBool, time::Instant};

use reqwest::Client as ReqwestClient;
use rusqlite::Connection;
use serenity::all::UserId;
use tokio::sync::Mutex;

use crate::config::Config;

pub(crate) struct BotState {
    pub(crate) config: Config,
    pub(crate) db: Mutex<Connection>,
    pub(crate) panel_lock: Mutex<()>,
    pub(crate) started: AtomicBool,
    pub(crate) ai_http: ReqwestClient,
    pub(crate) ai_cooldowns: Mutex<HashMap<UserId, Instant>>,
}

impl BotState {
    pub(crate) fn new(config: Config, db: Connection, ai_http: ReqwestClient) -> Self {
        Self {
            config,
            db: Mutex::new(db),
            panel_lock: Mutex::new(()),
            started: AtomicBool::new(false),
            ai_http,
            ai_cooldowns: Mutex::new(HashMap::new()),
        }
    }
}
