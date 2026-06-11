pub const PROMPTER_TEMPLATE: &str = r#"You are the Planner. Produce a structured implementation plan from the spec.

Rules:
1. Be fast — no more than 3 tool calls (read/grep) total
2. Do NOT use the task tool — read files directly
3. Only check files directly relevant to the spec

Read the spec, check 1-2 key files if needed, then output ONLY this JSON:

{"prompt": "Goal: ...\n\nContext: ...\n\nPlan: ...\n\nRequirements:\n1. ...\n\nConstraints:\n- ...", "rubric": [{"criterion": "...", "description": "...", "weight": 3}]}

---
---SPEC---
{{spec}}
---END SPEC---
---

Output ONLY the JSON. No markdown fences, no commentary.
"#;

pub const EVALUATOR_TEMPLATE: &str = r#"You are the Evaluator. Check if the Coder's implementation meets the spec and rubric.

Rules:
1. Be fast — max 3 read/grep calls
2. No task tool — read files directly
3. Check only files the coder claimed to have created/modified

---SPEC---
{{spec}}
---END SPEC---

---RUBRIC---
{{rubric}}
---END RUBRIC---

Output ONLY this JSON:
{"approved": true, "feedback": "ok", "diagnosis": "All criteria met", "results": [{"criterion": "...", "pass": true, "evidence": "..."}]}

- approved: true ONLY if ALL criteria pass
- feedback: concise explanation of what must be fixed
- diagnosis: technical root cause

No markdown fences, no extra text, valid JSON only.
"#;

pub const PROMPTER_REVISION_TEMPLATE: &str = r#"You are the Planner. The previous implementation failed. Produce a revised prompt.

Rules:
1. Be fast — no new exploration, use the previous output to understand context
2. No task tool
3. Fix what the evaluation says is broken

---SPEC---
{{spec}}
---END SPEC---

---PREVIOUS EVALUATION---
{{eval_feedback}}
{{eval_diagnosis}}
---END EVALUATION---

---CODER OUTPUT---
{{coder_output}}
---END CODER OUTPUT---

Output ONLY this JSON:
{"prompt": "Goal: ...\n\nContext: ...\n\nPlan: ...\n\nRequirements:\n1. ...\n\nConstraints:\n- ...", "rubric": [{"criterion": "...", "description": "...", "weight": 3}], "analysis": "Brief explanation of what was wrong and how this revision fixes it."}

No markdown fences, no extra text, valid JSON only.
"#;

pub fn render(template: &str, ctx: &std::collections::HashMap<&str, &str>) -> String {
    let mut result = template.to_string();

    loop {
        let before = result.clone();
        let mut in_if = false;
        let mut if_var = String::new();
        let mut if_start = 0;
        let mut if_depth = 0;

        for (i, _) in result.char_indices() {
            if result[i..].starts_with("{{#if ") {
                if_start = i;
                if_depth = 1;
                let rest = &result[i + 6..];
                if let Some(end) = rest.find("}}") {
                    if_var = rest[..end].trim().to_string();
                    in_if = true;
                }
            } else if in_if && result[i..].starts_with("{{#if ") {
                if_depth += 1;
            } else if in_if && result[i..].starts_with("{{/if}}") {
                if_depth -= 1;
                if if_depth == 0 {
                    let block_start = if_start;
                    let block_end = i + 7;
                    let inner_start = if_start + 6 + if_var.len() + 2;
                    let inner_end = i;
                    let inner = &result[inner_start..inner_end];

                    let has_var =
                        ctx.contains_key(if_var.as_str()) && !ctx.get(if_var.as_str()).unwrap_or(&"").is_empty();
                    if has_var {
                        result = result[..block_start].to_string() + inner + &result[block_end..];
                    } else {
                        result = result[..block_start].to_string() + &result[block_end..];
                    }
                    break;
                }
            }
        }
        if result == before {
            break;
        }
    }

    for (key, value) in ctx {
        result = result.replace(&format!("{{{{{}}}}}", key), value);
    }

    result
}

pub fn tail(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let truncated: String = text
        .chars()
        .rev()
        .take(max_bytes)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("[... truncated]\n{}", truncated)
}

pub fn sanitize_llm_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;

    for ch in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
                out.push(ch);
            } else if ch == '\\' {
                escaped = true;
                out.push(ch);
            } else if ch == '"' {
                in_string = false;
                out.push(ch);
            } else if ch == '\n' || ch == '\r' {
                out.push_str("\\n");
            } else if ch == '\t' {
                out.push_str("\\t");
            } else if ch.is_control() {
            } else {
                out.push(ch);
            }
        } else {
            if ch == '"' {
                in_string = true;
            }
            out.push(ch);
        }
    }
    out
}

pub fn extract_fenced_json(text: &str) -> Option<&str> {
    let start_marker = "```json\n";
    let end_marker = "\n```";

    if let (Some(s), Some(e)) = (text.rfind(start_marker), text.rfind(end_marker)) {
        if s < e {
            return Some(&text[s + start_marker.len()..e]);
        }
    }
    if let (Some(s), Some(e)) = (text.rfind("```\n"), text.rfind("\n```")) {
        if s < e {
            return Some(&text[s + 4..e]);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_simple() {
        let mut ctx = std::collections::HashMap::new();
        ctx.insert("name", "world");
        let result = render("Hello, {{name}}!", &ctx);
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn test_render_if_block_present() {
        let mut ctx = std::collections::HashMap::new();
        ctx.insert("show", "yes");
        ctx.insert("content", "visible");
        let result = render("before{{#if show}}{{content}}{{/if}}after", &ctx);
        assert_eq!(result, "beforevisibleafter");
    }

    #[test]
    fn test_render_if_block_absent() {
        let ctx = std::collections::HashMap::new();
        let result = render("before{{#if missing}}hidden{{/if}}after", &ctx);
        assert_eq!(result, "beforeafter");
    }

    #[test]
    fn test_extract_fenced_json() {
        let text = "Some text\n```json\n{\"key\": \"value\"}\n```\nmore";
        assert_eq!(extract_fenced_json(text), Some("{\"key\": \"value\"}"));
    }

    #[test]
    fn test_extract_fenced_json_no_fence() {
        assert!(extract_fenced_json("just text").is_none());
    }

    #[test]
    fn test_prompter_template_has_spec_placeholder() {
        assert!(PROMPTER_TEMPLATE.contains("{{spec}}"));
    }

    #[test]
    fn test_evaluator_template_has_required_placeholders() {
        assert!(EVALUATOR_TEMPLATE.contains("{{spec}}"));
        assert!(EVALUATOR_TEMPLATE.contains("{{rubric}}"));
    }

    #[test]
    fn test_prompter_revision_template_has_required_placeholders() {
        assert!(PROMPTER_REVISION_TEMPLATE.contains("{{spec}}"));
        assert!(PROMPTER_REVISION_TEMPLATE.contains("{{coder_output}}"));
        assert!(PROMPTER_REVISION_TEMPLATE.contains("{{eval_feedback}}"));
        assert!(PROMPTER_REVISION_TEMPLATE.contains("{{eval_diagnosis}}"));
    }
}
