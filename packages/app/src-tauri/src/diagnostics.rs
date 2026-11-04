//! The newest app log lines that contain a needle.

#[tauri::command]
pub fn app_log_lines(needle: String, max: usize) -> Vec<String> {
    let hits: Vec<String> = crate::flight::applog::tail().into_iter().filter(|l| l.contains(&needle)).collect();
    hits[hits.len().saturating_sub(max)..].to_vec()
}
