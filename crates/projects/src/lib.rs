//! turin-projects: server-side project services (see PORTING.md for upstream mapping).

pub mod agent_session_importer;
pub mod agent_session_json;
pub mod agent_session_scanner;
pub mod keyed_lock;
pub mod managed_project_folders;
pub mod project_enrichment_service;
pub mod project_favicon_resolver;
pub mod project_folder_name;
pub mod project_mutation;
pub mod project_path;
pub mod project_ports;
pub mod project_service;
pub mod repository_identity_resolver;
pub mod testing;
pub mod ttl_cache;
