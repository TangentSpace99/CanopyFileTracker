// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

//! Canopy engine: a persistent, diff-updated index of disk usage.

pub mod db;
pub mod query;
pub mod scan;
pub mod win;

pub use rusqlite;
