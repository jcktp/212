//! Header extras: system load (CPU / memory) and optional weather.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::proto::{Event, NoticeLevel};

use crate::{Internal, Runtime};

/// PID of the running backend (0 = none), shared with the stats task.
pub(crate) type BackendPid = Arc<AtomicU32>;

impl Runtime {
    pub(crate) fn start_background_tasks(&mut self) {
        if self.config.ui.system_stats {
            let events = self.events.clone();
            let pid = self.backend_pid.clone();
            tokio::spawn(stats_loop(events, pid));
        }
        self.restart_weather();
    }

    fn restart_weather(&mut self) {
        if let Some(t) = self.weather_task.take() {
            t.abort();
        }
        let Some(place) = self.config.ui.weather.clone().filter(|p| !p.trim().is_empty()) else { return };
        let fahrenheit = self.config.ui.weather_units == "fahrenheit";
        let tx = self.internal_tx.clone();
        let client = self.http.clone();
        self.weather_task = Some(tokio::spawn(async move {
            loop {
                let w = match weather(&client, &place, fahrenheit).await {
                    Ok(s) => Some(s),
                    Err(e) => {
                        tracing::warn!("weather: {e}");
                        None
                    }
                };
                if tx.send(Internal::Weather(w)).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(30 * 60)).await;
            }
        }));
    }

    pub(crate) async fn set_weather(&mut self, location: Option<String>) {
        let loc = location.map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && l != "off");
        self.save_setting("ui", "weather", loc.clone().unwrap_or_default().into());
        self.config.ui.weather = loc.clone();
        match loc {
            Some(l) => {
                self.notice(
                    NoticeLevel::Info,
                    format!("Weather for {l} from open-meteo.com (only the place name is sent). /weather off disables it."),
                )
                .await;
                self.restart_weather();
            }
            None => {
                if let Some(t) = self.weather_task.take() {
                    t.abort();
                }
                self.emit(Event::Weather(None)).await;
                self.notice(NoticeLevel::Info, "Weather disabled.").await;
            }
        }
    }
}

async fn stats_loop(events: tokio::sync::mpsc::Sender<Event>, backend: BackendPid) {
    use sysinfo::{CpuRefreshKind, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
    let Ok(mut sys) = System::new_with_specifics(
        RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_usage()).with_memory(MemoryRefreshKind::nothing().with_ram()),
    ) else {
        return;
    };
    let mut last: Option<(u32, u64, Option<u64>)> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        let pid = backend.load(Ordering::Relaxed);
        let model_mem = if pid != 0 {
            let p = Pid::from_u32(pid);
            sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[p]), true, ProcessRefreshKind::nothing().with_memory());
            sys.process(p).map(|p| p.memory())
        } else {
            None
        };
        let cpu = sys.global_cpu_usage();
        let (used, total) = (sys.used_memory(), sys.total_memory());
        // Only wake the UI when the displayed values change.
        let key = (cpu.round() as u32, used / (100 << 20), model_mem.map(|m| m / (100 << 20)));
        if last != Some(key) {
            last = Some(key);
            if events.send(Event::Stats { cpu_percent: cpu, mem_used: used, mem_total: total, model_mem }).await.is_err() {
                return;
            }
        }
    }
}

/// Current conditions for a place name via Open-Meteo (no API key).
pub async fn weather(client: &reqwest::Client, place: &str, fahrenheit: bool) -> Result<String, String> {
    let geo_url = reqwest::Url::parse_with_params(
        "https://geocoding-api.open-meteo.com/v1/search",
        &[("name", place), ("count", "1"), ("format", "json")],
    )
    .map_err(|e| e.to_string())?;
    let geo: serde_json::Value = client.get(geo_url).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let r = geo.pointer("/results/0").ok_or_else(|| format!("place '{place}' not found"))?;
    let (lat, lon) = (r["latitude"].as_f64().unwrap_or(0.0), r["longitude"].as_f64().unwrap_or(0.0));
    let name = r["name"].as_str().unwrap_or(place).to_string();
    let unit = if fahrenheit { "fahrenheit" } else { "celsius" };
    let url = reqwest::Url::parse_with_params(
        "https://api.open-meteo.com/v1/forecast",
        &[
            ("latitude", lat.to_string()),
            ("longitude", lon.to_string()),
            ("current", "temperature_2m,weather_code".to_string()),
            ("temperature_unit", unit.to_string()),
        ],
    )
    .map_err(|e| e.to_string())?;
    let w: serde_json::Value = client.get(url).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let t = w.pointer("/current/temperature_2m").and_then(|v| v.as_f64()).ok_or("no current weather")?;
    let code = w.pointer("/current/weather_code").and_then(|v| v.as_u64()).unwrap_or(0);
    Ok(format!("{} {:.0}°{} {name}", weather_label(code), t, if fahrenheit { "F" } else { "C" }))
}

/// WMO weather code → short label (the UI maps it to a glyph).
pub fn weather_label(code: u64) -> &'static str {
    match code {
        0 => "clear",
        1 | 2 => "partly",
        3 => "cloudy",
        45 | 48 => "fog",
        51..=57 => "drizzle",
        61..=67 | 80..=82 => "rain",
        71..=77 | 85 | 86 => "snow",
        95..=99 => "storm",
        _ => "cloudy",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn weather_codes() {
        assert_eq!(super::weather_label(0), "clear");
        assert_eq!(super::weather_label(63), "rain");
        assert_eq!(super::weather_label(96), "storm");
    }
}
