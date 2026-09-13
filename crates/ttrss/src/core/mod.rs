//! Pure domain core ported from the tt-rss PHP reference (no HTTP).
//!
//! Byte-for-byte ports of the PHP primitives the server depends on: PHP
//! serialization, DB-backed encrypted sessions, user password/OTP/app-password
//! verification, and the article-filter evaluation engine.

pub mod config;
pub mod crypt;
pub mod db;
pub mod feed;
pub mod filters;
pub mod phpser;
pub mod session;
pub mod user;
