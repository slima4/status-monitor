//! Public-status page assembly + cache.
//!
//! Pure status-mapping rules + a single-flight cache + a live aggregator that
//! builds the page payload from PostgreSQL and ClickHouse.

pub mod aggregator;
pub mod badge;
pub mod cache;
pub mod incident_writer;
pub mod overall_status;
pub mod source;
pub mod xml;

pub use aggregator::{AggregatorConfig, OrgAggregator};
pub use cache::{PageCache, PageCacheError, PageData};
pub use incident_writer::{
    InMemoryIncidentStore, IncidentStore, IncidentWriter, IncidentWriterConfig, NewOpenIncident,
    OpenIncident, PgIncidentStore,
};
pub use overall_status::{
    Counters, component_status, day_state, overall_label, overall_state, overall_status,
};
pub use source::{IncidentListQuery, NoopPublicSource, OrgPublicSource, PublicSource};
