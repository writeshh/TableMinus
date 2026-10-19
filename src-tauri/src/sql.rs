//! The SQL engines (Postgres, MySQL, SQLite and the rest behind `Driver`) and what the UI does on them: running SQL,
//! the sidebar's catalog, browsing, saving rows, structure and DDL, objects, the ER diagram, processes and users.

pub(crate) mod browse;
pub(crate) mod catalog;
pub(crate) mod db;
pub(crate) mod ddl;
pub(crate) mod diagram;
pub(crate) mod driver;
pub(crate) mod objects;
pub(crate) mod processes;
pub(crate) mod rows;
pub(crate) mod structure;
pub(crate) mod users;
