//! Probe which models the current API key can actually call. Sends a tiny request to each model on its supported
//! endpoint and prints status + latency. `melon probe-models [--json out.json]`
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

async fn probe(c: reqwest::Client, base: String, key: String, sem: Arc<tokio::sync::Semaphore>, m: Value) -> Value {
    let id = m["id"].as_str().unwrap_or("").to_string();
    let messages = m["supported_endpoints"].as_array().map_or(false, |e| e.iter().any(|x| x == "/messages"));
    let (url, body) = if messages {
        (format!("{base}/messages"), json!({"model": id, "max_tokens": 16, "messages": [{"role": "user", "content": "Reply with: ok"}]}))
    } else {
        (format!("{base}/chat/completions"), json!({"model": id, "max_tokens": 64, "messages": [{"role": "user", "content": "Reply with: ok"}]}))
    };
    let _p = sem.acquire().await;
    let t = Instant::now();
    match c.post(&url).bearer_auth(&key).header("x-api-key", &key).header("anthropic-version", "2023-06-01").json(&body).timeout(Duration::from_secs(60)).send().await {
        Ok(r) => {
            let status = r.status().as_u16();
            let ok = status == 200;
            let err = if ok { String::new() } else { r.text().await.unwrap_or_default().replace('\n', " ").chars().take(120).collect() };
            json!({"id": id, "endpoint": url.rsplit('/').next().unwrap_or(""), "ok": ok, "status": status, "sec": (t.elapsed().as_secs_f64() * 100.0).round() / 100.0, "err": err})
        }
        Err(e) => json!({"id": id, "ok": false, "status": 0, "sec": 0, "err": e.to_string().chars().take(120).collect::<String>()}),
    }
}

pub async fn main(argv: Vec<String>) -> i32 {
    crate::config::loader::load_dotenv(&crate::config::loader::home_dir().join(".env"));
    crate::config::loader::load_dotenv(&crate::config::loader::get_data_dir().join(".env"));
    let Ok(key) = std::env::var("API_KEY") else {
        eprintln!("API_KEY is not set");
        return 1;
    };
    let base = std::env::var("BASE_URL").unwrap_or_else(|_| crate::config::DEFAULT_BASE_URL.to_string());
    let c = reqwest::Client::new();
    let models: Value = match c.get(format!("{base}/models")).bearer_auth(&key).header("x-api-key", &key).send().await {
        Ok(r) => r.json().await.unwrap_or(Value::Null),
        Err(e) => {
            eprintln!("could not list models: {e}");
            return 1;
        }
    };
    let sem = Arc::new(tokio::sync::Semaphore::new(6));
    let futs = models["data"].as_array().cloned().unwrap_or_default().into_iter().map(|m| probe(c.clone(), base.clone(), key.clone(), sem.clone(), m));
    let res = futures::future::join_all(futs).await;
    for r in &res {
        println!("{} {} {:>5}s {:<45} {}", if r["ok"] == true { "OK " } else { "NO " }, r["status"], r["sec"], r["id"].as_str().unwrap_or(""), r["err"].as_str().unwrap_or(""));
    }
    println!("\n{}/{} callable", res.iter().filter(|r| r["ok"] == true).count(), res.len());
    if let Some(i) = argv.iter().position(|a| a == "--json") {
        if let Some(p) = argv.get(i + 1) {
            let _ = std::fs::write(p, serde_json::to_string_pretty(&res).unwrap_or_default());
        }
    }
    0
}
