//! The engines that aren't SQL: Redis, `MongoDB`, `DynamoDB`, Elasticsearch and Cassandra, each with its own
//! commands, and their open sessions.

pub(crate) mod cassandra;
pub(crate) mod dynamo;
pub(crate) mod elastic;
pub(crate) mod mongo;
pub(crate) mod redis;
pub(crate) mod sessions;
