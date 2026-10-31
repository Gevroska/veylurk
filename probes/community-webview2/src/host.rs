//! Native finite proof adapter; all browser behavior is shared with the source crate.
pub fn run(config: Option<community_source::sampling::Config>) {
    community_source::run_proof(config);
}
