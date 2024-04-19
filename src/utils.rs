use crate::config;
use quick_xml::{
    events::{BytesEnd, BytesStart, Event},
    Reader, Writer,
};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::borrow::Cow;
use std::io;
use std::io::Cursor;
use std::io::Write as IOWrite;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::time;

pub type Unix = std::time::Duration;

/// Refer to [`remove_xml_namespaces`] for an explanation.
pub const NAMESPACE_ALTER: &'static str = "__placeholder__";

#[allow(dead_code)]
pub fn log<S: AsRef<str>>(message: S) {
    let log_file_path = default_download_path().join("logfile");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file_path)
        .unwrap();
    writeln!(file, "{}", message.as_ref()).unwrap();
}

pub fn config_dir() -> PathBuf {
    let path = match std::env::var("XDG_CONFIG_HOME") {
        Ok(path) => PathBuf::from(path),
        Err(_) => dirs::home_dir()
            .expect("unable to locate home directory. Try setting 'XDG_CONFIG_HOME' manually")
            .join(".config"),
    }
    .join(crate::APPNAME);

    std::fs::create_dir_all(&path).unwrap();

    path
}

pub fn current_unix() -> Unix {
    let secs = chrono::Utc::now().timestamp() as u64;
    Unix::from_secs(secs)
}

pub fn default_download_path() -> PathBuf {
    let p = dirs::home_dir().unwrap().join(crate::APPNAME);
    std::fs::create_dir_all(&p).unwrap();
    p
}

pub fn get_guid(item: &serde_json::Map<String, Value>) -> &str {
    let guid_obj = item.get("guid").unwrap();
    if let Some(guid) = guid_obj.as_str() {
        return guid;
    }

    guid_obj
        .as_object()
        .unwrap()
        .get("#text")
        .unwrap()
        .as_str()
        .unwrap()
}

/// The quickxml_to_serde library merges tags that have same name but different namespaces.
/// This is not the behaviour i want, as users should be able to fetch specific names with
/// patterns. This is a hack to avoid it, by replacing the colon (which marks a namespace)
/// with a replacement symbol. When the user then queries a tag with a pattern,
/// we replace the colons in their pattern with the same replacement.
pub fn remove_xml_namespaces(xml: &str, replacement: &str) -> String {
    fn modify_name<'a>(original_name: &'a [u8], replacement: &'a str) -> Cow<'a, [u8]> {
        if let Some(pos) = original_name.iter().position(|&b| b == b':') {
            let mut new_name = Vec::from(&original_name[..pos]);
            new_name.extend_from_slice(replacement.as_bytes());
            new_name.extend_from_slice(&original_name[pos + 1..]);
            Cow::Owned(new_name)
        } else {
            Cow::Borrowed(original_name)
        }
    }

    let mut reader = Reader::from_str(xml);
    reader.trim_text(true);
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let modified_name = modify_name(name.as_ref(), replacement);
                let elem_name_str = String::from_utf8_lossy(&modified_name);
                let elem = BytesStart::new(elem_name_str.as_ref());
                writer
                    .write_event(Event::Start(elem))
                    .expect("Unable to write event");
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                let modified_name = modify_name(name.as_ref(), replacement);
                let elem_name_str = String::from_utf8_lossy(&modified_name);
                let elem = BytesEnd::new(elem_name_str.as_ref());
                writer
                    .write_event(Event::End(elem))
                    .expect("Unable to write event");
            }
            Ok(Event::Eof) => break,
            Ok(e) => writer.write_event(e).expect("Unable to write event"),
            Err(e) => panic!("Error at position {}: {:.?}", reader.buffer_position(), e),
        }
    }

    let result = writer.into_inner().into_inner();
    String::from_utf8(result).expect("Found invalid UTF-8")
}

pub fn truncate_string(s: &str, max_width: usize, append_dots: bool) -> String {
    let mut width = 0;
    let mut truncated = String::new();
    let mut reached_max = false;

    for c in s.chars() {
        let mut buf = [0; 4];
        let encoded_char = c.encode_utf8(&mut buf);
        let char_width = unicode_width::UnicodeWidthStr::width(encoded_char);
        if width + char_width > max_width {
            reached_max = true;
            break;
        }
        truncated.push(c);
        width += char_width;
    }

    if reached_max && append_dots {
        truncated.pop();
        truncated.pop();
        truncated.pop();

        truncated.push_str("...");
    }

    truncated
}

#[derive(Serialize)]
struct BasicPodcast {
    url: String,
}

pub fn handle_response(response: Result<reqwest::Response, reqwest::Error>) -> reqwest::Response {
    match response {
        Ok(res) => res,
        Err(e) => {
            let url = e.url().unwrap().clone();

            let error_message = match e {
                e if e.is_builder() => format!("Invalid URL: {}", url),
                e if e.is_connect() => format!(
                    "Failed to connect. Ensure you're connected to the internet: {}",
                    url
                ),
                e if e.is_timeout() => format!("Timeout reached for URL: {}", url),
                e if e.is_status() => format!("Server error {}: {}", e.status().unwrap(), url),
                e if e.is_redirect() => format!("Too many redirects for URL: {}", url),
                e if e.is_decode() => format!("Failed to decode response from URL: {}", url),
                _ => format!("An unexpected error occurred: {}", e),
            };
            eprintln!("{}", error_message);
            process::exit(1);
        }
    }
}

pub async fn download_text(client: &reqwest::Client, url: &str) -> String {
    let response = client.get(url).send().await;
    let response = handle_response(response);

    match response.text().await {
        Ok(text) => text,
        Err(e) => {
            eprintln!("failed to decode response from url: {}\nerror:{}", url, e);
            process::exit(1);
        }
    }
}

pub fn edit_file(path: &Path) {
    if !path.exists() {
        eprintln!("error: path does not exist: {:?}", path);
    }

    let editor = match std::env::var("EDITOR") {
        Ok(editor) => editor,
        Err(_) => {
            eprintln!("Unable to edit {:?}", path);
            eprintln!("Please configure your $EDITOR environment variable");
            std::process::exit(1);
        }
    };

    std::process::Command::new(editor)
        .arg(path.to_str().unwrap())
        .status()
        .unwrap();
}

pub fn replacer(val: Value, input: &str) -> String {
    let mut inside = false;
    let mut output = String::new();
    let mut pattern = String::new();
    for c in input.chars() {
        if c == '{' {
            if inside {
                panic!();
            } else {
                inside = true;
            }
        } else if c == '}' {
            if !inside {
                panic!();
            } else {
                let p = std::mem::take(&mut pattern);
                let mut replacement = val
                    .get(&p)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("<<{}>>", p))
                    .replace("\\", "");
                replacement.pop();
                replacement.remove(0);
                output.push_str(&replacement);
                inside = false;
            }
        } else {
            if inside {
                pattern.push(c);
            } else {
                output.push(c);
            }
        }
    }

    output
}

pub async fn search_podcasts(config: &config::GlobalConfig, query: String, catch_up: bool) {
    let response = search(&query).await;
    let mut results = vec![];

    let mut idx = 0;
    for res in response.into_iter() {
        results.push(res);
        idx += 1;
        if idx == config.max_search_results() {
            break;
        }
    }

    if results.is_empty() {
        eprintln!("no podcasts matched your query.");
        return;
    }

    eprintln!("Enter index of podcast to add");
    for (idx, res) in results.iter().enumerate() {
        let line = replacer(res.clone(), &config.search.pattern());
        let line = format!("{}: {}", idx, line);
        let line = truncate_string(&line, config.max_line_width(), true);
        println!("{}", line);
    }

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let input = input.trim();

    if input.is_empty() {
        return;
    }

    let mut indices = vec![];
    for input in input.split(" ") {
        let Ok(num) = input.parse::<usize>() else {
            eprintln!(
                "invalid input: {}. You must enter the index of a podcast",
                input
            );
            return;
        };

        if num > results.len() || num == 0 {
            eprintln!("index {} is out of bounds", num);
            return;
        }

        indices.push(num - 1);
    }

    let mut regex_parts = vec![];
    for index in indices {
        let url = results[index].get("feedUrl").unwrap().to_string();
        let name = results[index].get("artistName").unwrap().to_string();

        let podcast = config::PodcastConfig::new(url);

        if config::PodcastConfigs::push(name.clone(), podcast) {
            eprintln!("'{}' added!", name);
            if catch_up {
                regex_parts.push(format!("^{}$", &name));
            }
        } else {
            eprintln!("'{}' already exists!", name);
        }
    }

    if catch_up && !regex_parts.is_empty() {
        let regex = regex_parts.join("|");
        let filter = Regex::new(&regex).unwrap();
        config::PodcastConfigs::catch_up(Some(filter));
    }
}

pub fn date_str_to_unix(date: &str) -> time::Duration {
    let secs = dateparser::parse(date).unwrap().timestamp();
    time::Duration::from_secs(secs as u64)
}

pub fn get_extension_from_response(response: &reqwest::Response, url: &str) -> String {
    let ext = match PathBuf::from(url)
        .extension()
        .and_then(|ext| ext.to_str().map(String::from))
    {
        Some(ext) => ext.to_string(),
        None => {
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|ct| ct.to_str().ok())
                .unwrap_or("application/octet-stream");

            let extensions = mime_guess::get_mime_extensions_str(&content_type).unwrap();

            match extensions.contains(&"mp3") {
                true => "mp3".to_owned(),
                false => extensions
                    .first()
                    .expect("extension not found.")
                    .to_string(),
            }
        }
    };

    // Some urls have these arguments after the extension.
    // feels a bit hacky.
    // todo: find a cleaner way to extract extensions.
    let ext = ext
        .split_once("?")
        .map(|(l, _)| l.to_string())
        .unwrap_or(ext);
    ext
}

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

pub async fn search(terms: &str) -> Vec<Value> {
    let encoded: String = utf8_percent_encode(terms, NON_ALPHANUMERIC).to_string();
    let url = format!(
        "https://itunes.apple.com/search?media=podcast&entity=podcast&term={}",
        encoded
    );
    let resp = reqwest::get(&url).await.unwrap().text().await.unwrap();

    serde_json::from_str::<serde_json::Value>(&resp)
        .unwrap()
        .get("results")
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modify_xml_tags() {
        let xml = r#"<root><foo:bar>Content</foo:bar><baz:qux>More Content</baz:qux></root>"#;
        let replacement = "___placeholder___";

        let expected = r#"<root><foo___placeholder___bar>Content</foo___placeholder___bar><baz___placeholder___qux>More Content</baz___placeholder___qux></root>"#;

        let modified_xml = remove_xml_namespaces(xml, replacement);

        assert_eq!(
            modified_xml, expected,
            "The modified XML does not match the expected output."
        );
    }
}
