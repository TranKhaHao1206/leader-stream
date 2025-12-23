use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gloo_timers::callback::{Interval, Timeout};
use js_sys::{Function, Object, Reflect};
use serde::Deserialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    Document, Element, EventSource, HtmlAnchorElement, HtmlInputElement, HtmlTableRowElement,
    HtmlTableSectionElement, MessageEvent,
};

const DEFAULT_LIMIT: i64 = 5000;
const MOBILE_MAX_LIMIT: i64 = 200;
const DEFAULT_SLOT_MS: f64 = 400.0;
const AUTO_REFRESH_INTERVAL_MS: u32 = 30000;
const ETA_REFRESH_INTERVAL_MS: u32 = 1000;
const ROW_ANIMATION_MS: u32 = 180;
const COPY_FEEDBACK_MS: u32 = 1200;
const SCROLL_RESUME_DELAY_MS: u32 = 300;
const DONATE_COPY_RESET_MS: u32 = 1400;

#[derive(Clone)]
struct LeaderRow {
    start_slot: i64,
    end_slot: i64,
    range_start: i64,
    leader: String,
    tpu: Option<String>,
    ip: Option<String>,
    port: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LeaderSlot {
    slot: i64,
    leader: String,
    tpu: Option<String>,
    ip: Option<String>,
    port: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NextLeadersResponse {
    current_slot: i64,
    limit: i64,
    leaders: Option<Vec<LeaderSlot>>,
    slot_ms: Option<f64>,
    ts: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamPayload {
    slot: i64,
}

struct InitialConfig {
    limit: i64,
    window_override: Option<i64>,
}

struct AppState {
    document: Document,
    status_el: Option<Element>,
    meta_el: Option<Element>,
    table_body: Option<HtmlTableSectionElement>,
    table_wrap: Option<Element>,
    donate_button: Option<Element>,
    filter_input: Option<HtmlInputElement>,
    filter_summary: Option<Element>,
    leader_stream_url: String,
    stream_source: Option<EventSource>,
    pending_slot: Option<i64>,
    leaders_by_slot: HashMap<i64, LeaderRow>,
    row_by_slot: HashMap<i64, HtmlTableRowElement>,
    current_slot: Option<i64>,
    last_limit: i64,
    last_fetch_ts: Option<f64>,
    window_override: Option<i64>,
    max_visible_rows: i64,
    visible_slot_count: i64,
    slot_ms: f64,
    last_slot_update_ts: Option<f64>,
    auto_refresh_timer: Option<Interval>,
    eta_timer: Option<Interval>,
    is_loading: bool,
    is_paused: bool,
    donate_reset_timer: Option<Timeout>,
    scroll_resume_timer: Option<Timeout>,
    filter_prefix: String,
    copy_reset_timers: HashMap<u32, Timeout>,
    next_copy_id: u32,
}

impl AppState {
    fn new(document: Document) -> Self {
        let status_el = document.get_element_by_id("status");
        let meta_el = document.get_element_by_id("meta");
        let table_body = document
            .query_selector("#leaders-table tbody")
            .ok()
            .flatten()
            .and_then(|el| el.dyn_into::<HtmlTableSectionElement>().ok());
        let table_wrap = document.query_selector(".table-wrap").ok().flatten();
        let donate_button = document.query_selector(".donate-link").ok().flatten();
        let filter_input = document
            .get_element_by_id("leader-filter")
            .and_then(|el| el.dyn_into::<HtmlInputElement>().ok());
        let filter_summary = document.get_element_by_id("filter-summary");
        let leader_stream_url = resolve_leader_stream_url(&document);

        Self {
            document,
            status_el,
            meta_el,
            table_body,
            table_wrap,
            donate_button,
            filter_input,
            filter_summary,
            leader_stream_url,
            stream_source: None,
            pending_slot: None,
            leaders_by_slot: HashMap::new(),
            row_by_slot: HashMap::new(),
            current_slot: None,
            last_limit: DEFAULT_LIMIT,
            last_fetch_ts: None,
            window_override: None,
            max_visible_rows: DEFAULT_LIMIT,
            visible_slot_count: 0,
            slot_ms: DEFAULT_SLOT_MS,
            last_slot_update_ts: None,
            auto_refresh_timer: None,
            eta_timer: None,
            is_loading: false,
            is_paused: false,
            donate_reset_timer: None,
            scroll_resume_timer: None,
            filter_prefix: String::new(),
            copy_reset_timers: HashMap::new(),
            next_copy_id: 1,
        }
    }
}

fn window() -> web_sys::Window {
    web_sys::window().expect("window")
}

fn detect_touch_device(window: &web_sys::Window) -> bool {
    let has_touch = Reflect::has(window.as_ref(), &JsValue::from_str("ontouchstart"))
        .unwrap_or(false);
    let navigator = window.navigator();
    let max_touch_points = navigator.max_touch_points();
    let ms_max_touch_points = Reflect::get(&navigator, &JsValue::from_str("msMaxTouchPoints"))
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    has_touch || max_touch_points > 0 || ms_max_touch_points > 0.0
}

fn parse_limit(value: &str) -> i64 {
    let parsed = value.parse::<i64>().unwrap_or(DEFAULT_LIMIT);
    parsed.clamp(1, 5000)
}

fn get_initial_config(window: &web_sys::Window, is_touch_device: bool) -> InitialConfig {
    let search = window.location().search().unwrap_or_default();
    let params = web_sys::UrlSearchParams::new_with_str(&search)
        .unwrap_or_else(|_| web_sys::UrlSearchParams::new_with_str("").unwrap());

    let limit_param = params.get("limit");
    let window_param = params.get("window").or_else(|| params.get("max"));

    let mut limit = limit_param.as_deref().map(parse_limit).unwrap_or(DEFAULT_LIMIT);
    let mut window_override = window_param.as_deref().map(parse_limit);

    if is_touch_device {
        limit = limit.min(MOBILE_MAX_LIMIT);
        if let Some(value) = window_override.as_mut() {
            *value = (*value).min(MOBILE_MAX_LIMIT);
        }
    }

    InitialConfig {
        limit,
        window_override,
    }
}

fn resolve_leader_stream_url(document: &Document) -> String {
    let meta = document
        .query_selector("meta[name=\"leader-stream-url\"]")
        .ok()
        .flatten();
    if let Some(meta) = meta {
        if let Some(value) = meta.get_attribute("content") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    "/api/leader-stream".to_string()
}

fn read_initial_payload(document: &Document) -> Option<NextLeadersResponse> {
    let el = document.get_element_by_id("initial-data")?;
    let text = el.text_content().unwrap_or_default();
    if text.trim().is_empty() {
        return None;
    }
    let value = js_sys::JSON::parse(&text).ok()?;
    let payload: NextLeadersResponse = serde_wasm_bindgen::from_value(value).ok()?;
    el.remove();
    Some(payload)
}

fn set_status(state: &AppState, text: &str, tone: &str) {
    if let Some(status_el) = &state.status_el {
        status_el.set_text_content(Some(text));
        let _ = status_el.set_attribute("data-tone", tone);
    }
}

fn normalize_prefix(value: &str) -> String {
    value.trim().to_string()
}

fn row_matches_prefix(row: &LeaderRow, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    row.leader.starts_with(prefix)
}

fn format_time(ts: f64) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(ts));
    let options = Object::new();
    let _ = Reflect::set(&options, &JsValue::from_str("hour"), &JsValue::from_str("2-digit"));
    let _ = Reflect::set(&options, &JsValue::from_str("minute"), &JsValue::from_str("2-digit"));
    let _ = Reflect::set(&options, &JsValue::from_str("second"), &JsValue::from_str("2-digit"));
    let func = Reflect::get(date.as_ref(), &JsValue::from_str("toLocaleTimeString"))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok());
    if let Some(func) = func {
        if let Ok(value) = func.call2(date.as_ref(), &JsValue::UNDEFINED, &options) {
            if let Some(text) = value.as_string() {
                return text;
            }
        }
    }
    "--:--:--".to_string()
}

fn format_slots_until(state: &AppState, slot: i64) -> String {
    match state.current_slot {
        Some(current_slot) => {
            let diff = slot - current_slot;
            if diff <= 0 {
                "0".to_string()
            } else {
                diff.to_string()
            }
        }
        None => "-".to_string(),
    }
}

fn get_effective_slot_ms(state: &AppState) -> f64 {
    if state.slot_ms.is_finite() && state.slot_ms > 0.0 {
        return state.slot_ms;
    }
    DEFAULT_SLOT_MS
}

fn ms_until_slot(state: &AppState, slot: i64) -> Option<f64> {
    let current_slot = state.current_slot?;
    let diff_slots = slot - current_slot;
    let elapsed = state
        .last_slot_update_ts
        .map(|ts| js_sys::Date::now() - ts)
        .unwrap_or(0.0);
    Some((diff_slots as f64) * get_effective_slot_ms(state) - elapsed)
}

fn format_end_time(state: &AppState, end_slot: i64) -> String {
    match ms_until_slot(state, end_slot + 1) {
        None => "-".to_string(),
        Some(ms) => {
            if ms <= 0.0 {
                "now".to_string()
            } else {
                format_time(js_sys::Date::now() + ms)
            }
        }
    }
}

fn update_meta(state: &mut AppState) {
    let meta_el = match &state.meta_el {
        Some(el) => el,
        None => return,
    };
    let current_slot = match state.current_slot {
        Some(value) => value,
        None => {
            meta_el.set_text_content(Some(""));
            return;
        }
    };

    let shown = state.visible_slot_count.max(0);
    let total = if state.max_visible_rows > 0 {
        state.max_visible_rows
    } else if state.last_limit > 0 {
        state.last_limit
    } else {
        shown
    };
    let stamp = state
        .last_fetch_ts
        .map(format_time)
        .unwrap_or_else(|| "--:--:--".to_string());
    let filter_note = if state.filter_prefix.is_empty() {
        ""
    } else {
        " (filtered)"
    };
    let text = format!(
        "Slot {} - {}/{} slots{} - {}",
        current_slot, shown, total, filter_note, stamp
    );
    meta_el.set_text_content(Some(&text));
}

fn normalize_table_layout(state: &AppState) {
    if let Ok(Some(head_row)) = state
        .document
        .query_selector("#leaders-table thead tr")
    {
        head_row.set_inner_html(
            "<th>Slots until</th>\
             <th>Slot start</th>\
             <th class=\"eta-col\">Ends (est.)</th>\
             <th class=\"leader-col\"><span class=\"th-label\">Leader</span><span class=\"th-pause-pill\">Paused</span></th>\
             <th>TPU</th>",
        );
    }
    if let Ok(Some(cell)) = state
        .document
        .query_selector("#leaders-table tbody td.empty")
    {
        let _ = cell.set_attribute("colspan", "5");
    }
}

fn update_filter_summary(state: &mut AppState) {
    let summary_el = match &state.filter_summary {
        Some(el) => el,
        None => return,
    };

    if state.filter_prefix.is_empty() {
        summary_el.set_text_content(Some("All leaders"));
        return;
    }

    if state.leaders_by_slot.is_empty() {
        summary_el.set_text_content(Some("No leaders loaded"));
        return;
    }

    let label = if state.visible_slot_count == 1 { "slot" } else { "slots" };
    let text = format!("{} {} match", state.visible_slot_count, label);
    summary_el.set_text_content(Some(&text));
}

fn update_status_loaded(state: &AppState, force: bool) {
    let status_el = match &state.status_el {
        Some(el) => el,
        None => return,
    };

    if !force {
        if let Some(tone) = status_el.get_attribute("data-tone") {
            if tone == "error" {
                return;
            }
        }
    }

    let filter_note = if state.filter_prefix.is_empty() {
        ""
    } else {
        " (filtered)"
    };
    let text = format!(
        "Showing {} next slots{}",
        state.visible_slot_count.max(0),
        filter_note
    );
    status_el.set_text_content(Some(&text));
    let _ = status_el.set_attribute("data-tone", "success");
}

fn clear_table(state: &mut AppState, message: &str) {
    if let Some(table_body) = &state.table_body {
        table_body.set_inner_html(&format!(
            "<tr><td colspan=\"5\" class=\"empty\">{}</td></tr>",
            message
        ));
    }
    state.leaders_by_slot.clear();
    state.row_by_slot.clear();
    state.visible_slot_count = 0;
    update_filter_summary(state);
}

fn resolve_window_size(state: &AppState, limit: i64, fallback_length: i64) -> i64 {
    let effective_limit = if limit > 0 { limit } else { fallback_length };
    if effective_limit <= 0 {
        return fallback_length;
    }
    if let Some(window_override) = state.window_override {
        return window_override.min(effective_limit);
    }
    effective_limit
}

fn limit_rows(state: &mut AppState, rows: Vec<LeaderSlot>) -> Vec<LeaderSlot> {
    let window_size = resolve_window_size(state, state.last_limit, rows.len() as i64);
    state.max_visible_rows = window_size.max(0);
    rows.into_iter().take(state.max_visible_rows as usize).collect()
}

fn apply_filter_to_table(state: &mut AppState) {
    let mut count = 0i64;
    for (start_slot, row) in state.leaders_by_slot.iter() {
        let matches = row_matches_prefix(row, &state.filter_prefix);
        if let Some(tr) = state.row_by_slot.get(start_slot) {
            let _ = tr
                .class_list()
                .toggle_with_force("row-filtered", !matches);
        }
        if matches {
            if row.end_slot >= row.start_slot {
                count += row.end_slot - row.start_slot + 1;
            }
        }
    }
    state.visible_slot_count = count;
    update_meta(state);
    update_filter_summary(state);
    update_status_loaded(state, false);
    update_active_row(state);
}

fn update_active_row(state: &AppState) {
    let table_body = match &state.table_body {
        Some(body) => body,
        None => return,
    };

    if let Ok(rows) = table_body.query_selector_all("tr.row-active") {
        for index in 0..rows.length() {
            if let Some(node) = rows.item(index) {
                if let Ok(row) = node.dyn_into::<HtmlTableRowElement>() {
                    let _ = row.class_list().remove_1("row-active");
                }
            }
        }
    }

    if let Ok(rows) = table_body.query_selector_all("tr[data-slot]") {
        for index in 0..rows.length() {
            if let Some(node) = rows.item(index) {
                if let Ok(row) = node.dyn_into::<HtmlTableRowElement>() {
                    let class_list = row.class_list();
                    if class_list.contains("row-exit") || class_list.contains("row-filtered") {
                        continue;
                    }
                    let _ = class_list.add_1("row-active");
                    break;
                }
            }
        }
    }
}

fn group_leaders(rows: &[LeaderSlot]) -> Vec<LeaderRow> {
    group_leaders_with_limit(rows, None)
}

fn group_leaders_with_limit(
    rows: &[LeaderSlot],
    max_group_len: Option<i64>,
) -> Vec<LeaderRow> {
    if rows.is_empty() {
        return Vec::new();
    }

    let mut grouped: Vec<LeaderRow> = Vec::new();

    for row in rows {
        if let Some(current_row) = grouped.last_mut() {
            let same_leader =
                row.leader == current_row.leader && row.slot == current_row.end_slot + 1;
            let can_extend = same_leader
                && max_group_len.map_or(true, |max_len| {
                    (current_row.end_slot - current_row.start_slot + 1) < max_len
                });
            if can_extend {
                current_row.end_slot = row.slot;
                if current_row.tpu.is_none() {
                    current_row.tpu = row.tpu.clone();
                }
                if current_row.ip.is_none() {
                    current_row.ip = row.ip.clone();
                }
                if current_row.port.is_none() {
                    current_row.port = row.port.clone();
                }
                continue;
            }
        }

        let new_row = LeaderRow {
            start_slot: row.slot,
            end_slot: row.slot,
            range_start: row.slot,
            leader: row.leader.clone(),
            tpu: row.tpu.clone(),
            ip: row.ip.clone(),
            port: row.port.clone(),
        };
        grouped.push(new_row);
    }

    grouped
}

fn set_copy_cell(cell: &Element, value: Option<&str>, display_value: &str) {
    cell.set_text_content(Some(display_value));
    let _ = cell.set_attribute("data-copy-value", value.unwrap_or(""));
    let _ = cell.set_attribute("data-copy-display", display_value);

    if let Some(value) = value {
        if !value.is_empty() {
            let _ = cell.class_list().add_1("copyable");
            let _ = cell.set_attribute("role", "button");
            let _ = cell.set_attribute("tabindex", "0");
            let _ = cell.set_attribute("title", "Click to copy");
            return;
        }
    }

    let _ = cell.class_list().remove_1("copyable");
    let _ = cell.remove_attribute("role");
    let _ = cell.remove_attribute("tabindex");
    let _ = cell.remove_attribute("title");
}

fn apply_row_data(state: &AppState, tr: &HtmlTableRowElement, row: &LeaderRow) {
    let tpu = row.tpu.clone().unwrap_or_else(|| "-".to_string());

    let _ = tr.set_attribute("data-slot", &row.start_slot.to_string());
    let _ = tr.set_attribute("data-end-slot", &row.end_slot.to_string());

    if let Ok(Some(cell)) = tr.query_selector(".cell-until") {
        cell.set_text_content(Some(&format_slots_until(state, row.range_start)));
    }
    if let Ok(Some(cell)) = tr.query_selector(".cell-slot") {
        cell.set_text_content(Some(&row.range_start.to_string()));
    }
    if let Ok(Some(cell)) = tr.query_selector(".cell-eta") {
        let value = format_end_time(state, row.end_slot);
        cell.set_text_content(Some(&value));
    }
    if let Ok(Some(cell)) = tr.query_selector(".cell-leader") {
        cell.set_text_content(Some(""));
        if let Ok(link_el) = state.document.create_element("a") {
            if let Ok(link) = link_el.dyn_into::<HtmlAnchorElement>() {
                link.set_class_name("leader-link");
                link.set_href(&format!(
                    "https://www.validators.app/validators?q={}&network=mainnet",
                    js_sys::encode_uri_component(&row.leader)
                ));
                link.set_target("_blank");
                let _ = link.set_attribute("rel", "noreferrer");
                link.set_text_content(Some(&row.leader));
                let _ = cell.append_child(&link);
            }
        }
    }
    if let Ok(Some(cell)) = tr.query_selector(".cell-tpu") {
        set_copy_cell(&cell, row.tpu.as_deref(), &tpu);
    }
}

fn create_row(state: &AppState, row: &LeaderRow) -> Option<HtmlTableRowElement> {
    let tr = state
        .document
        .create_element("tr")
        .ok()?
        .dyn_into::<HtmlTableRowElement>()
        .ok()?;
    let _ = tr.class_list().add_1("row-enter");
    tr.set_inner_html(
        "<td class=\"mono cell-until\" data-label=\"Slots until\"></td>\
         <td class=\"mono cell-slot\" data-label=\"Slot start\"></td>\
         <td class=\"mono eta cell-eta\" data-label=\"Ends (est.)\"></td>\
         <td class=\"mono cell-leader\" data-label=\"Leader\"></td>\
         <td class=\"mono cell-tpu\" data-label=\"TPU\"></td>",
    );

    apply_row_data(state, &tr, row);

    let tr_clone = tr.clone();
    let closure = Closure::wrap(Box::new(move |_ts: f64| {
        let _ = tr_clone.class_list().remove_1("row-enter");
    }) as Box<dyn FnMut(f64)>);
    let _ = window().request_animation_frame(closure.as_ref().unchecked_ref());
    closure.forget();

    Some(tr)
}

fn insert_row_in_order(state: &AppState, tr: &HtmlTableRowElement, slot: i64) {
    let table_body = match &state.table_body {
        Some(body) => body,
        None => return,
    };

    if let Ok(rows) = table_body.query_selector_all("tr[data-slot]") {
        for index in 0..rows.length() {
            if let Some(node) = rows.item(index) {
                if let Ok(row) = node.dyn_into::<HtmlTableRowElement>() {
                    if row.class_list().contains("row-exit") {
                        continue;
                    }
                    if let Some(row_slot) = row.get_attribute("data-slot") {
                        if let Ok(row_slot) = row_slot.parse::<i64>() {
                            if row_slot > slot {
                                let _ = table_body.insert_before(tr, Some(&row));
                                return;
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = table_body.append_child(tr);
}

fn update_eta_cells(state: &mut AppState) {
    if state.is_paused || state.current_slot.is_none() {
        return;
    }
    let table_body = match &state.table_body {
        Some(body) => body,
        None => return,
    };
    if let Ok(rows) = table_body.query_selector_all("tr[data-slot]") {
        for index in 0..rows.length() {
            if let Some(node) = rows.item(index) {
                if let Ok(row) = node.dyn_into::<HtmlTableRowElement>() {
                    if row.class_list().contains("row-exit") {
                        continue;
                    }
                    if let Some(end_slot) = row.get_attribute("data-end-slot") {
                        if let Ok(end_slot) = end_slot.parse::<i64>() {
                            if let Ok(Some(cell)) = row.query_selector(".cell-eta") {
                                let value = format_end_time(state, end_slot);
                                cell.set_text_content(Some(&value));
                            }
                        }
                    }
                    if let Some(start_slot) = row.get_attribute("data-slot") {
                        if let Ok(start_slot) = start_slot.parse::<i64>() {
                            if let Ok(Some(cell)) = row.query_selector(".cell-until") {
                                cell.set_text_content(Some(&format_slots_until(state, start_slot)));
                            }
                        }
                    }
                }
            }
        }
    }
}

fn remove_row(state: &mut AppState, slot: i64) {
    let row = match state.row_by_slot.remove(&slot) {
        Some(row) => row,
        None => {
            state.leaders_by_slot.remove(&slot);
            return;
        }
    };
    state.leaders_by_slot.remove(&slot);

    let _ = row.class_list().add_1("row-exit");
    let row_clone = row.clone();
    let closure = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if row_clone.is_connected() {
            row_clone.remove();
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    let _ = row.add_event_listener_with_callback("transitionend", closure.as_ref().unchecked_ref());
    closure.forget();

    let row_clone = row.clone();
    let timeout = Timeout::new(ROW_ANIMATION_MS + 50, move || {
        if row_clone.is_connected() {
            row_clone.remove();
        }
    });
    timeout.forget();
}

fn render_table(state: &mut AppState, rows: &[LeaderRow]) {
    if rows.is_empty() {
        clear_table(state, "No leaders returned for this range.");
        return;
    }
    let table_body = match &state.table_body {
        Some(body) => body,
        None => return,
    };

    state.leaders_by_slot.clear();
    state.row_by_slot.clear();
    table_body.set_inner_html("");

    let fragment = state.document.create_document_fragment();
    for row in rows {
        state.leaders_by_slot.insert(row.start_slot, row.clone());
        if let Some(tr) = create_row(state, row) {
            state.row_by_slot.insert(row.start_slot, tr.clone());
            let _ = fragment.append_child(&tr);
        }
    }

    let _ = table_body.append_child(&fragment);
    apply_filter_to_table(state);
}

fn reconcile_leaders(state: &mut AppState, rows: &[LeaderRow]) {
    if rows.is_empty() {
        clear_table(state, "No leaders returned for this range.");
        return;
    }

    if state.row_by_slot.is_empty() {
        render_table(state, rows);
        return;
    }

    let incoming_slots: HashSet<i64> = rows.iter().map(|row| row.start_slot).collect();
    let existing_slots: Vec<i64> = state.leaders_by_slot.keys().cloned().collect();

    for start_slot in existing_slots {
        if !incoming_slots.contains(&start_slot) {
            remove_row(state, start_slot);
        }
    }

    for row in rows {
        state.leaders_by_slot.insert(row.start_slot, row.clone());
        if let Some(existing_row) = state.row_by_slot.get(&row.start_slot) {
            apply_row_data(state, existing_row, row);
            continue;
        }

        if let Some(tr) = create_row(state, row) {
            state.row_by_slot.insert(row.start_slot, tr.clone());
            insert_row_in_order(state, &tr, row.start_slot);
        }
    }

    apply_filter_to_table(state);
    update_eta_cells(state);
}

fn build_params(limit: i64) -> String {
    format!("limit={}", limit)
}

async fn fetch_json(url: &str) -> Result<JsValue, JsValue> {
    let response = JsFuture::from(window().fetch_with_str(url)).await?;
    let response: web_sys::Response = response.dyn_into()?;
    if !response.ok() {
        let text = JsFuture::from(response.text()?).await?;
        let message = text.as_string().unwrap_or_else(|| "Request failed".to_string());
        return Err(JsValue::from_str(&message));
    }
    JsFuture::from(response.json()?).await
}

async fn fetch_leaders_data(limit: i64) -> Result<NextLeadersResponse, JsValue> {
    let url = format!("/api/next-leaders?{}", build_params(limit));
    let value = fetch_json(&url).await?;
    serde_wasm_bindgen::from_value(value).map_err(|err| JsValue::from_str(&err.to_string()))
}

fn js_error_message(err: JsValue, fallback: &str) -> String {
    if let Some(message) = err.as_string() {
        return message;
    }
    if let Ok(error) = err.dyn_into::<js_sys::Error>() {
        return error.message().into();
    }
    fallback.to_string()
}

async fn write_to_clipboard(text: &str) -> Result<bool, JsValue> {
    let navigator = window().navigator();
    let has_clipboard =
        Reflect::has(&navigator, &JsValue::from_str("clipboard")).unwrap_or(false);
    if has_clipboard {
        let clipboard = navigator.clipboard();
        let promise = clipboard.write_text(text);
        if JsFuture::from(promise).await.is_ok() {
            return Ok(true);
        }
    }

    let document = window().document().expect("document");
    let textarea = document
        .create_element("textarea")?
        .dyn_into::<web_sys::HtmlTextAreaElement>()?;
    textarea.set_value(text);
    textarea.set_attribute("readonly", "")?;
    textarea
        .style()
        .set_property("position", "absolute")?;
    textarea.style().set_property("left", "-9999px")?;
    let _ = document.body().unwrap().append_child(&textarea);
    textarea.select();

    let exec = Reflect::get(document.as_ref(), &JsValue::from_str("execCommand"))?;
    let success = if exec.is_function() {
        let func: Function = exec.dyn_into()?;
        let result = func.call1(document.as_ref(), &JsValue::from_str("copy"))?;
        result.as_bool().unwrap_or(false)
    } else {
        false
    };
    textarea.remove();
    Ok(success)
}

fn ensure_copy_id(state: &mut AppState, cell: &Element) -> u32 {
    if let Some(value) = cell.get_attribute("data-copy-id") {
        if let Ok(parsed) = value.parse::<u32>() {
            return parsed;
        }
    }
    let id = state.next_copy_id;
    state.next_copy_id += 1;
    let _ = cell.set_attribute("data-copy-id", &id.to_string());
    id
}

fn handle_copy_cell(state_rc: Rc<RefCell<AppState>>, cell: Element) {
    let value = cell.get_attribute("data-copy-value");
    let value = match value {
        Some(value) if !value.is_empty() => value,
        _ => return,
    };

    let original = cell
        .get_attribute("data-copy-display")
        .or_else(|| cell.text_content())
        .unwrap_or_default();

    spawn_local(async move {
        let copied = write_to_clipboard(&value).await.unwrap_or(false);
        cell.set_text_content(Some(if copied { "Copied!" } else { "Copy failed" }));

        let mut state = state_rc.borrow_mut();
        let id = ensure_copy_id(&mut state, &cell);
        state.copy_reset_timers.remove(&id);
        let state_clone = state_rc.clone();
        let cell_clone = cell.clone();
        let original_clone = original.clone();
        let timer = Timeout::new(COPY_FEEDBACK_MS, move || {
            cell_clone.set_text_content(Some(&original_clone));
            if let Ok(mut state) = state_clone.try_borrow_mut() {
                state.copy_reset_timers.remove(&id);
            }
        });
        state.copy_reset_timers.insert(id, timer);
    });
}

fn copy_donate_address(state_rc: Rc<RefCell<AppState>>) {
    let donate_button = {
        let state = state_rc.borrow();
        state.donate_button.clone()
    };
    let donate_button = match donate_button {
        Some(button) => button,
        None => return,
    };
    let address = donate_button.get_attribute("data-address");
    let address = match address {
        Some(address) if !address.is_empty() => address,
        _ => return,
    };
    let original_text = donate_button.text_content().unwrap_or_default();

    spawn_local(async move {
        let copied = write_to_clipboard(&address).await.unwrap_or(false);
        donate_button.set_text_content(Some(if copied {
            "Solana address copied!"
        } else {
            "Copy failed"
        }));

        let state_clone = state_rc.clone();
        let button_clone = donate_button.clone();
        let original_clone = original_text.clone();
        let timeout = Timeout::new(DONATE_COPY_RESET_MS, move || {
            button_clone.set_text_content(Some(&original_clone));
            if let Ok(mut state) = state_clone.try_borrow_mut() {
                state.donate_reset_timer = None;
            }
        });

        let mut state = state_rc.borrow_mut();
        state.donate_reset_timer = Some(timeout);
    });
}

fn stop_auto_update(state: &mut AppState) {
    state.auto_refresh_timer.take();
    state.eta_timer.take();
}

fn pause_updates(state_rc: Rc<RefCell<AppState>>) {
    let mut state = state_rc.borrow_mut();
    if state.is_paused {
        return;
    }
    state.is_paused = true;
    set_paused_indicator(&state, true);
}

fn resume_updates(state_rc: Rc<RefCell<AppState>>) {
    let pending_slot = {
        let mut state = state_rc.borrow_mut();
        if !state.is_paused {
            return;
        }
        state.is_paused = false;
        set_paused_indicator(&state, false);
        state.pending_slot.take()
    };

    let mut state = state_rc.borrow_mut();
    if let Some(slot) = pending_slot {
        apply_slot_update(&mut state, slot);
    } else {
        update_eta_cells(&mut state);
    }
}

fn set_paused_indicator(state: &AppState, paused: bool) {
    let target = state.table_wrap.clone().or_else(|| {
        state
            .table_body
            .as_ref()
            .map(|body| body.clone().unchecked_into::<Element>())
    });
    let target = match target {
        Some(target) => target,
        None => return,
    };
    if paused {
        let _ = target.class_list().add_1("is-paused");
    } else {
        let _ = target.class_list().remove_1("is-paused");
    }
}

fn apply_slot_update(state: &mut AppState, slot: i64) {
    if let Some(current_slot) = state.current_slot {
        if slot <= current_slot {
            return;
        }
    }
    state.current_slot = Some(slot);
    state.last_slot_update_ts = Some(js_sys::Date::now());
    prune_leaders(state, slot);
    update_eta_cells(state);
}

fn handle_stream_slot(state_rc: Rc<RefCell<AppState>>, slot: i64) {
    let mut state = state_rc.borrow_mut();
    if state.is_paused {
        state.pending_slot = Some(slot);
        return;
    }
    apply_slot_update(&mut state, slot);
}

fn start_slot_stream(state_rc: Rc<RefCell<AppState>>) {
    let stream_url = {
        let state = state_rc.borrow();
        state.leader_stream_url.clone()
    };

    let event_source = match EventSource::new(&stream_url) {
        Ok(source) => source,
        Err(err) => {
            let state = state_rc.borrow();
            let message = js_error_message(err, "Slot stream unavailable");
            set_status(&state, &format!("Slot stream error: {}", message), "error");
            return;
        }
    };

    let onmessage_state = state_rc.clone();
    let onmessage = Closure::wrap(Box::new(move |event: MessageEvent| {
        let data = match event.data().as_string() {
            Some(data) => data,
            None => return,
        };
        let value = match js_sys::JSON::parse(&data) {
            Ok(value) => value,
            Err(_) => return,
        };
        let payload: StreamPayload = match serde_wasm_bindgen::from_value(value) {
            Ok(payload) => payload,
            Err(_) => return,
        };
        handle_stream_slot(onmessage_state.clone(), payload.slot);
    }) as Box<dyn FnMut(MessageEvent)>);
    event_source.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();

    state_rc.borrow_mut().stream_source = Some(event_source);
}

fn handle_scroll_activity(state_rc: Rc<RefCell<AppState>>) {
    pause_updates(state_rc.clone());
    {
        let mut state = state_rc.borrow_mut();
        state.scroll_resume_timer = None;
    }
    let state_clone = state_rc.clone();
    let timeout = Timeout::new(SCROLL_RESUME_DELAY_MS, move || {
        resume_updates(state_clone.clone());
    });
    state_rc.borrow_mut().scroll_resume_timer = Some(timeout);
}

fn start_auto_update(state_rc: Rc<RefCell<AppState>>) {
    {
        let mut state = state_rc.borrow_mut();
        stop_auto_update(&mut state);
    }

    let auto_refresh_state = state_rc.clone();
    let auto_refresh_timer = Interval::new(AUTO_REFRESH_INTERVAL_MS, move || {
        let limit = {
            let state = auto_refresh_state.borrow();
            if state.is_paused {
                return;
            }
            state.last_limit
        };
        fetch_leaders(auto_refresh_state.clone(), limit, true);
    });

    let eta_state = state_rc.clone();
    let eta_timer = Interval::new(ETA_REFRESH_INTERVAL_MS, move || {
        let mut state = eta_state.borrow_mut();
        if !state.is_paused {
            update_eta_cells(&mut state);
        }
    });

    let mut state = state_rc.borrow_mut();
    state.auto_refresh_timer = Some(auto_refresh_timer);
    state.eta_timer = Some(eta_timer);
}

fn apply_leaders_payload(
    state: &mut AppState,
    data: NextLeadersResponse,
    requested_limit: i64,
    silent: bool,
) {
    let rows = data.leaders.unwrap_or_default();
    let incoming_slot = data.current_slot;
    let should_update_slot = match state.current_slot {
        Some(current) => incoming_slot > current,
        None => true,
    };
    if should_update_slot {
        state.current_slot = Some(incoming_slot);
        state.last_slot_update_ts = Some(js_sys::Date::now());
    }

    let mut effective_limit = requested_limit.max(1);
    if data.limit > 0 {
        effective_limit = effective_limit.min(data.limit);
    }
    state.last_limit = effective_limit;
    state.last_fetch_ts = Some(data.ts);
    state.max_visible_rows =
        resolve_window_size(state, state.last_limit, state.max_visible_rows);

    if let Some(slot_ms) = data.slot_ms {
        if slot_ms.is_finite() && slot_ms > 0.0 {
            state.slot_ms = slot_ms;
        }
    }

    let limited_rows = limit_rows(state, rows);
    let grouped_rows = group_leaders(&limited_rows);
    reconcile_leaders(state, &grouped_rows);
    if let Some(current_slot) = state.current_slot {
        prune_leaders(state, current_slot);
        update_eta_cells(state);
    }

    update_status_loaded(state, true);
}

fn fetch_leaders(state_rc: Rc<RefCell<AppState>>, limit: i64, silent: bool) {
    let should_fetch = {
        let mut state = state_rc.borrow_mut();
        if state.is_loading {
            false
        } else {
            state.is_loading = true;
            if !silent {
                set_status(&state, "Fetching leader schedule...", "info");
                clear_table(&mut state, "Loading...");
            }
            true
        }
    };

    if !should_fetch {
        return;
    }

    let state_clone = state_rc.clone();
    spawn_local(async move {
        let result = fetch_leaders_data(limit).await;
        match result {
            Ok(data) => {
                let mut state = state_clone.borrow_mut();
                apply_leaders_payload(&mut state, data, limit, silent);

                state.is_loading = false;
                drop(state);
                start_auto_update(state_clone.clone());
            }
            Err(err) => {
                let message = js_error_message(err, "Request failed");
                let mut state = state_clone.borrow_mut();
                set_status(&state, &format!("Error: {}", message), "error");
                if !silent {
                    clear_table(&mut state, "Unable to load leaders.");
                }
                state.is_loading = false;
            }
        }
    });
}

fn prune_leaders(state: &mut AppState, next_slot: i64) {
    if state.row_by_slot.is_empty() {
        return;
    }

    let keys: Vec<i64> = state.leaders_by_slot.keys().cloned().collect();
    for start_slot in keys {
        let row = match state.leaders_by_slot.get(&start_slot).cloned() {
            Some(row) => row,
            None => continue,
        };
        if row.end_slot < next_slot {
            remove_row(state, start_slot);
            continue;
        }

        if next_slot > row.start_slot {
            let mut updated = row.clone();
            updated.start_slot = next_slot;
            state.leaders_by_slot.remove(&start_slot);
            state.leaders_by_slot.insert(updated.start_slot, updated.clone());
            if let Some(tr) = state.row_by_slot.remove(&start_slot) {
                state.row_by_slot.insert(updated.start_slot, tr.clone());
                apply_row_data(state, &tr, &updated);
            }
        }
    }

    apply_filter_to_table(state);
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    let window = window();
    let document = window.document().expect("document");
    let is_touch_device = detect_touch_device(&window);
    let initial_payload = read_initial_payload(&document);
    let state = AppState::new(document);
    let state_rc = Rc::new(RefCell::new(state));

    {
        let state = state_rc.borrow();
        normalize_table_layout(&state);
    }

    let initial_config = get_initial_config(&window, is_touch_device);
    {
        let mut state = state_rc.borrow_mut();
        state.window_override = initial_config.window_override;
        state.max_visible_rows = state
            .window_override
            .map(|window_override| window_override.min(initial_config.limit))
            .unwrap_or(initial_config.limit);
        if let Some(input) = &state.filter_input {
            state.filter_prefix = normalize_prefix(&input.value());
            update_filter_summary(&mut state);
        }
    }

    if let Some(payload) = initial_payload {
        let payload_limit = payload.limit.max(0);
        let should_fetch_more =
            initial_config.limit > payload_limit && initial_config.limit > 0;
        {
            let mut state = state_rc.borrow_mut();
            apply_leaders_payload(&mut state, payload, initial_config.limit, false);
        }
        if should_fetch_more {
            fetch_leaders(state_rc.clone(), initial_config.limit, true);
        } else {
            start_auto_update(state_rc.clone());
        }
    } else {
        fetch_leaders(state_rc.clone(), initial_config.limit, false);
    }
    start_slot_stream(state_rc.clone());

    let pause_target = {
        let state = state_rc.borrow();
        state
            .table_wrap
            .clone()
            .or_else(|| {
                state
                    .table_body
                    .as_ref()
                    .map(|body| body.clone().unchecked_into::<Element>())
            })
    };
    if let Some(target) = pause_target {
        for event_name in [
            "mouseenter",
            "mouseleave",
            "focusin",
            "focusout",
            "pointerenter",
            "pointerleave",
        ] {
            let state_clone = state_rc.clone();
            let handler = Closure::wrap(Box::new(move |_event: web_sys::Event| {
                if event_name.contains("leave") || event_name.contains("out") {
                    resume_updates(state_clone.clone());
                } else {
                    pause_updates(state_clone.clone());
                }
            }) as Box<dyn FnMut(web_sys::Event)>);
            let _ = target.add_event_listener_with_callback(event_name, handler.as_ref().unchecked_ref());
            handler.forget();
        }
    }

    {
        let state = state_rc.borrow();
        if let Some(button) = &state.donate_button {
            let state_clone = state_rc.clone();
            let handler = Closure::wrap(Box::new(move |_event: web_sys::Event| {
                copy_donate_address(state_clone.clone());
            }) as Box<dyn FnMut(web_sys::Event)>);
            let _ = button.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref());
            handler.forget();
        }
    }

    if is_touch_device {
        let state_clone = state_rc.clone();
        let handler = Closure::wrap(Box::new(move |_event: web_sys::Event| {
            handle_scroll_activity(state_clone.clone());
        }) as Box<dyn FnMut(web_sys::Event)>);
        let _ = window.add_event_listener_with_callback("scroll", handler.as_ref().unchecked_ref());
        handler.forget();

        let state_clone = state_rc.clone();
        let handler = Closure::wrap(Box::new(move |_event: web_sys::Event| {
            pause_updates(state_clone.clone());
        }) as Box<dyn FnMut(web_sys::Event)>);
        let _ = window.add_event_listener_with_callback("touchstart", handler.as_ref().unchecked_ref());
        handler.forget();

        let state_clone = state_rc.clone();
        let handler = Closure::wrap(Box::new(move |_event: web_sys::Event| {
            handle_scroll_activity(state_clone.clone());
        }) as Box<dyn FnMut(web_sys::Event)>);
        let _ = window.add_event_listener_with_callback("touchend", handler.as_ref().unchecked_ref());
        handler.forget();
    }

    {
        let state = state_rc.borrow();
        if let Some(input) = &state.filter_input {
            let state_clone = state_rc.clone();
            let handler = Closure::wrap(Box::new(move |event: web_sys::Event| {
                let target = event
                    .target()
                    .and_then(|target| target.dyn_into::<HtmlInputElement>().ok());
                if let Some(target) = target {
                    let mut state = state_clone.borrow_mut();
                    state.filter_prefix = normalize_prefix(&target.value());
                    apply_filter_to_table(&mut state);
                }
            }) as Box<dyn FnMut(web_sys::Event)>);
            let _ = input.add_event_listener_with_callback("input", handler.as_ref().unchecked_ref());
            handler.forget();

            let state_clone = state_rc.clone();
            let handler = Closure::wrap(Box::new(move |event: web_sys::Event| {
                let key_event = match event.dyn_into::<web_sys::KeyboardEvent>() {
                    Ok(event) => event,
                    Err(_) => return,
                };
                if key_event.key() != "Escape" {
                    return;
                }
                if let Some(target) = key_event
                    .target()
                    .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
                {
                    target.set_value("");
                }
                let mut state = state_clone.borrow_mut();
                state.filter_prefix = String::new();
                apply_filter_to_table(&mut state);
            }) as Box<dyn FnMut(web_sys::Event)>);
            let _ = input.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
            handler.forget();
        }
    }

    {
        let state = state_rc.borrow();
        if let Some(table_body) = &state.table_body {
            let state_clone = state_rc.clone();
            let handler = Closure::wrap(Box::new(move |event: web_sys::Event| {
                let target = event
                    .target()
                    .and_then(|target| target.dyn_into::<Element>().ok());
                let cell = target
                    .and_then(|target| target.closest(".cell-tpu").ok())
                    .flatten();
                if let Some(cell) = cell {
                    handle_copy_cell(state_clone.clone(), cell);
                }
            }) as Box<dyn FnMut(web_sys::Event)>);
            let _ = table_body.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref());
            handler.forget();

            let state_clone = state_rc.clone();
            let handler = Closure::wrap(Box::new(move |event: web_sys::Event| {
                let key_event = match event.dyn_into::<web_sys::KeyboardEvent>() {
                    Ok(event) => event,
                    Err(_) => return,
                };
                if key_event.key() != "Enter" && key_event.key() != " " {
                    return;
                }
                let target = key_event
                    .target()
                    .and_then(|target| target.dyn_into::<Element>().ok());
                let cell = target
                    .and_then(|target| target.closest(".cell-tpu").ok())
                    .flatten();
                if let Some(cell) = cell {
                    key_event.prevent_default();
                    handle_copy_cell(state_clone.clone(), cell);
                }
            }) as Box<dyn FnMut(web_sys::Event)>);
            let _ = table_body.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
            handler.forget();
        }
    }

    Ok(())
}
