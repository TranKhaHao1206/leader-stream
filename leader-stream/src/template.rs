const INDEX_TEMPLATE: &str = include_str!("index.html");
const DOCS_TEMPLATE: &str = include_str!("docs.html");
const MAP_TEMPLATE: &str = include_str!("map.html");
const LEADER_STREAM_TOKEN: &str = "{{leader_stream_url}}";
const INITIAL_SCRIPT_TOKEN: &str = "{{initial_script}}";
const CACHE_BUST_TOKEN: &str = "{{cache_bust}}";

pub fn render_index(
    leader_stream_url: &str,
    cache_bust: &str,
    initial_payload: Option<&str>,
) -> String {
    let initial_script = initial_payload
        .map(|payload| {
            format!(
                r#"    <script id=\"initial-data\" type=\"application/json\">{}</script>"#,
                payload
            )
        })
        .unwrap_or_default();

    INDEX_TEMPLATE
        .replace(LEADER_STREAM_TOKEN, leader_stream_url)
        .replace(CACHE_BUST_TOKEN, cache_bust)
        .replace(INITIAL_SCRIPT_TOKEN, &initial_script)
}

pub fn render_docs(cache_bust: &str) -> String {
    DOCS_TEMPLATE.replace(CACHE_BUST_TOKEN, cache_bust)
}

pub fn render_map(cache_bust: &str, leader_stream_url: &str) -> String {
    MAP_TEMPLATE
        .replace(CACHE_BUST_TOKEN, cache_bust)
        .replace(LEADER_STREAM_TOKEN, leader_stream_url)
}
