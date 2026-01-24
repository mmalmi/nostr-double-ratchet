use serde::Serialize;

/// Output formatter that supports both human-readable and JSON output
pub struct Output {
    json: bool,
}

impl Output {
    pub fn new(json: bool) -> Self {
        Self { json }
    }

    /// Output a successful result
    pub fn success<T: Serialize>(&self, command: &str, data: T) {
        if self.json {
            let response = JsonResponse {
                status: "ok",
                command,
                data: Some(data),
                error: None::<String>,
            };
            println!("{}", serde_json::to_string(&response).unwrap());
        } else {
            // For human output, serialize nicely
            println!("{}", serde_json::to_string_pretty(&data).unwrap());
        }
    }

    /// Output a simple success message
    pub fn success_message(&self, command: &str, message: &str) {
        if self.json {
            let response = JsonResponse {
                status: "ok",
                command,
                data: Some(serde_json::json!({ "message": message })),
                error: None::<String>,
            };
            println!("{}", serde_json::to_string(&response).unwrap());
        } else {
            println!("{}", message);
        }
    }

    /// Output an error
    pub fn error(&self, message: &str) {
        if self.json {
            let response: JsonResponse<()> = JsonResponse {
                status: "error",
                command: "",
                data: None,
                error: Some(message.to_string()),
            };
            eprintln!("{}", serde_json::to_string(&response).unwrap());
        } else {
            eprintln!("Error: {}", message);
        }
    }

    /// Output a streaming event (for listen commands)
    #[allow(dead_code)]
    pub fn event<T: Serialize>(&self, event_type: &str, data: T) {
        if self.json {
            let event = StreamEvent {
                event: event_type,
                data,
            };
            println!("{}", serde_json::to_string(&event).unwrap());
        } else {
            println!("[{}] {}", event_type, serde_json::to_string_pretty(&data).unwrap());
        }
    }
}

#[derive(Serialize)]
struct JsonResponse<'a, T: Serialize> {
    status: &'a str,
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[allow(dead_code)]
#[derive(Serialize)]
struct StreamEvent<'a, T: Serialize> {
    event: &'a str,
    #[serde(flatten)]
    data: T,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_json_mode() {
        let output = Output::new(true);
        assert!(output.json);
    }

    #[test]
    fn test_output_human_mode() {
        let output = Output::new(false);
        assert!(!output.json);
    }
}
