//! Built-in test accounts.

/// What kind of account a [`TestUser`] is, which decides the login response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountKind {
    /// `auth:1`, `status:"Active"`, expiry one year from now.
    Active,
    /// Active, `exp_date: null` ("Unlimited").
    Unlimited,
    /// Active, `is_trial:"1"`, `max_connections:"1"`, expiry in 3 days.
    Trial,
    /// `auth:1`, `status:"Expired"`, `exp_date` in the past.
    Expired,
    /// `auth:1`, `status:"Disabled"`.
    Disabled,
    /// `auth:0`-style failure (`{"user_info":{"auth":0,"status":"Banned"}}`).
    Banned,
}

impl AccountKind {
    /// The `user_info.status` string the panel reports.
    pub fn status(self) -> &'static str {
        match self {
            Self::Active | Self::Unlimited | Self::Trial => "Active",
            Self::Expired => "Expired",
            Self::Disabled => "Disabled",
            Self::Banned => "Banned",
        }
    }

    /// Whether API actions and stream URLs work for this account.
    pub fn can_stream(self) -> bool {
        matches!(self, Self::Active | Self::Unlimited | Self::Trial)
    }
}

/// A username/password pair the fake panel accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TestUser {
    /// Username, sent as-is (all built-in usernames are plain ASCII).
    pub username: &'static str,
    /// Password; clients must percent-encode it in queries and path segments.
    pub password: &'static str,
    /// Account behaviour.
    pub kind: AccountKind,
}

/// The password of [`SPECIAL`]: contains `@ / # space % & +`, so clients must percent-encode.
pub const SPECIAL_PASSWORD: &str = "p@ss/w#rd %&+";

/// `demo` / `demo`: the normal, active account.
pub const DEMO: TestUser = TestUser {
    username: "demo",
    password: "demo",
    kind: AccountKind::Active,
};
/// `expired` / `expired`.
pub const EXPIRED: TestUser = TestUser {
    username: "expired",
    password: "expired",
    kind: AccountKind::Expired,
};
/// `banned` / `banned`.
pub const BANNED: TestUser = TestUser {
    username: "banned",
    password: "banned",
    kind: AccountKind::Banned,
};
/// `disabled` / `disabled`.
pub const DISABLED: TestUser = TestUser {
    username: "disabled",
    password: "disabled",
    kind: AccountKind::Disabled,
};
/// `trial` / `trial`.
pub const TRIAL: TestUser = TestUser {
    username: "trial",
    password: "trial",
    kind: AccountKind::Trial,
};
/// `unlimited` / `unlimited` (active, no expiry date).
pub const UNLIMITED: TestUser = TestUser {
    username: "unlimited",
    password: "unlimited",
    kind: AccountKind::Unlimited,
};
/// `special` / [`SPECIAL_PASSWORD`] (active).
pub const SPECIAL: TestUser = TestUser {
    username: "special",
    password: SPECIAL_PASSWORD,
    kind: AccountKind::Active,
};

/// Every built-in account.
pub const USERS: &[TestUser] = &[DEMO, EXPIRED, BANNED, DISABLED, TRIAL, UNLIMITED, SPECIAL];

/// Look up an account by exact (already-decoded) credentials.
pub fn find(username: &str, password: &str) -> Option<&'static TestUser> {
    USERS
        .iter()
        .find(|u| u.username == username && u.password == password)
}

/// Look up an account that may use stream URLs.
pub(crate) fn find_streamable(username: &str, password: &str) -> Option<&'static TestUser> {
    find(username, password).filter(|u| u.kind.can_stream())
}
