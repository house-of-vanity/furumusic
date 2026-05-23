use serde::{Deserialize, Serialize};

use super::dto::{FolderContext, NormalizedFields, PathHints, RawMetadata, SimilarArtist, SimilarRelease};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single message in the chat history.
#[derive(Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    response_format: ChatResponseFormat,
    stream: bool,
    temperature: f64,
}

#[derive(Serialize)]
struct ChatResponseFormat {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    model: Option<String>,
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Deserialize, Default)]
struct ChatUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

async fn call_llm_chat(
    base_url: &str,
    model: &str,
    messages: &[ChatMessage],
    auth: Option<&str>,
) -> anyhow::Result<(String, String, ChatUsage)> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;

    let request = ChatRequest {
        model: model.to_owned(),
        messages: messages.to_vec(),
        response_format: ChatResponseFormat {
            kind: "json_object".to_owned(),
        },
        stream: false,
        temperature: 0.1,
    };

    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    tracing::info!(
        %url,
        model,
        message_count = messages.len(),
        "Calling LLM API (chat mode)..."
    );

    let start = std::time::Instant::now();
    let mut req = client.post(&url).json(&request);
    if let Some(auth_header) = auth {
        req = req.header("Authorization", auth_header);
    }
    let resp = req.send().await?;
    let elapsed = start.elapsed();

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::error!(%status, body = &body[..body.len().min(500)], "LLM API error");
        anyhow::bail!("LLM returned {}: {}", status, body);
    }

    let chat_resp: ChatResponse = resp.json().await?;
    let resp_model = chat_resp.model.unwrap_or_else(|| model.to_owned());
    let usage = chat_resp.usage.unwrap_or_default();
    let content = chat_resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("LLM returned empty choices"))?
        .message
        .content;

    tracing::info!(
        elapsed_ms = elapsed.as_millis() as u64,
        response_len = content.len(),
        prompt_tokens = usage.prompt_tokens.unwrap_or(0),
        completion_tokens = usage.completion_tokens.unwrap_or(0),
        model = %resp_model,
        "LLM response received"
    );
    tracing::debug!(raw_response = %content, "LLM raw output");

    Ok((content, resp_model, usage))
}

// ---------------------------------------------------------------------------
// Batch normalize — process multiple files in one LLM call
// ---------------------------------------------------------------------------

/// Input for one file in a batch normalize call.
pub struct BatchFileInput {
    pub filename: String,
    pub raw: RawMetadata,
    pub hints: PathHints,
}

/// Result of a batch normalize call.
pub struct BatchNormalizeResult {
    /// (filename, normalized_fields) pairs.
    pub results: Vec<(String, NormalizedFields)>,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub duration_ms: u64,
}

/// Estimate the token count for a batch of files.
/// Uses the rough heuristic of 1 token per 4 characters.
fn estimate_batch_tokens(
    system_prompt: &str,
    files: &[BatchFileInput],
    similar_artists: &[SimilarArtist],
    similar_releases: &[SimilarRelease],
    folder_ctx: Option<&FolderContext>,
) -> u64 {
    let system_tokens = system_prompt.len() as u64 / 4;

    // Shared context (RAG + folder) — sent once
    let mut shared_chars: u64 = 0;
    for a in similar_artists {
        shared_chars += 40 + a.name.len() as u64;
    }
    for r in similar_releases {
        shared_chars += 50 + r.title.len() as u64;
    }
    if let Some(ctx) = folder_ctx {
        shared_chars += 60 + ctx.folder_path.len() as u64;
        for f in &ctx.folder_files {
            shared_chars += 4 + f.len() as u64;
        }
    }
    let shared_tokens = shared_chars / 4;

    // Per-file: metadata input + expected response
    let mut per_file_tokens: u64 = 0;
    for f in files {
        let mut chars: u64 = 40 + f.filename.len() as u64; // header
        if let Some(v) = &f.raw.title { chars += 10 + v.len() as u64; }
        if let Some(v) = &f.raw.artist { chars += 12 + v.len() as u64; }
        if let Some(v) = &f.raw.album { chars += 12 + v.len() as u64; }
        if f.raw.year.is_some() { chars += 12; }
        if f.raw.track_number.is_some() { chars += 18; }
        if let Some(v) = &f.raw.genre { chars += 10 + v.len() as u64; }
        // hints
        if let Some(v) = &f.hints.artist { chars += 16 + v.len() as u64; }
        if let Some(v) = &f.hints.album { chars += 16 + v.len() as u64; }
        if let Some(v) = &f.hints.title { chars += 15 + v.len() as u64; }
        if f.hints.year.is_some() { chars += 14; }
        if f.hints.track_number.is_some() { chars += 20; }
        per_file_tokens += chars / 4;
        // Expected response per file (~150 tokens)
        per_file_tokens += 150;
    }

    system_tokens + shared_tokens + per_file_tokens
}

/// Build the user message for a batch of files.
fn build_batch_user_message(
    files: &[BatchFileInput],
    similar_artists: &[SimilarArtist],
    similar_releases: &[SimilarRelease],
    folder_ctx: Option<&FolderContext>,
) -> String {
    let mut msg = String::with_capacity(4096);

    // Shared context first
    if let Some(ctx) = folder_ctx {
        msg.push_str("## Folder context\n");
        msg.push_str(&format!("Folder path: \"{}\"\n", ctx.folder_path));
        msg.push_str(&format!("Total files in folder: {}\n\n", ctx.track_count));
    }

    if !similar_artists.is_empty() {
        msg.push_str("## Existing artists in database\n");
        for a in similar_artists {
            msg.push_str(&format!("- \"{}\" (similarity: {:.2})\n", a.name, a.similarity));
        }
        msg.push('\n');
    }

    if !similar_releases.is_empty() {
        msg.push_str("## Existing releases in database\n");
        for r in similar_releases {
            let year_str = r.year.map(|y| format!(", year: {y}")).unwrap_or_default();
            msg.push_str(&format!("- \"{}\" (similarity: {:.2}{})\n", r.title, r.similarity, year_str));
        }
        msg.push('\n');
    }

    // Per-file metadata
    msg.push_str(&format!("## Files to process ({})\n\n", files.len()));

    for f in files {
        msg.push_str(&format!("### {}\n", f.filename));

        if let Some(v) = &f.raw.title { msg.push_str(&format!("Title: \"{v}\"\n")); }
        if let Some(v) = &f.raw.artist { msg.push_str(&format!("Artist: \"{v}\"\n")); }
        if let Some(v) = &f.raw.album { msg.push_str(&format!("Release: \"{v}\"\n")); }
        if let Some(v) = f.raw.year { msg.push_str(&format!("Year: {v}\n")); }
        if let Some(v) = f.raw.track_number { msg.push_str(&format!("Track: {v}\n")); }
        if let Some(v) = &f.raw.genre { msg.push_str(&format!("Genre: \"{v}\"\n")); }

        // Path hints (only if different from tag metadata)
        let has_hints = f.hints.artist.is_some()
            || f.hints.album.is_some()
            || f.hints.title.is_some()
            || f.hints.year.is_some()
            || f.hints.track_number.is_some();
        if has_hints {
            if let Some(v) = &f.hints.artist { msg.push_str(&format!("Path artist: \"{v}\"\n")); }
            if let Some(v) = &f.hints.album { msg.push_str(&format!("Path release: \"{v}\"\n")); }
            if let Some(v) = &f.hints.title { msg.push_str(&format!("Path title: \"{v}\"\n")); }
            if let Some(v) = f.hints.year { msg.push_str(&format!("Path year: {v}\n")); }
            if let Some(v) = f.hints.track_number { msg.push_str(&format!("Path track: {v}\n")); }
        }
        msg.push('\n');
    }

    msg
}

/// Normalize a batch of files in one LLM call.
/// If the batch is too large for the context window, it is automatically
/// split in half and each half is processed recursively.
pub async fn normalize_batch(
    llm_url: &str,
    llm_model: &str,
    llm_auth: &str,
    system_prompt: &str,
    context_limit: u64,
    files: Vec<BatchFileInput>,
    similar_artists: &[SimilarArtist],
    similar_releases: &[SimilarRelease],
    folder_ctx: Option<&FolderContext>,
) -> anyhow::Result<BatchNormalizeResult> {
    // Estimate tokens
    let estimated = estimate_batch_tokens(
        system_prompt, &files, similar_artists, similar_releases, folder_ctx,
    );

    // If over 80% of context limit and more than 1 file, split
    let limit_80 = context_limit * 80 / 100;
    if estimated > limit_80 && files.len() > 1 {
        tracing::info!(
            estimated_tokens = estimated,
            context_limit,
            file_count = files.len(),
            "Batch too large, splitting in half"
        );
        let mid = files.len() / 2;
        let mut files_vec = files;
        let right = files_vec.split_off(mid);
        let left = files_vec;

        let left_result = Box::pin(normalize_batch(
            llm_url, llm_model, llm_auth, system_prompt, context_limit,
            left, similar_artists, similar_releases, folder_ctx,
        )).await?;

        let right_result = Box::pin(normalize_batch(
            llm_url, llm_model, llm_auth, system_prompt, context_limit,
            right, similar_artists, similar_releases, folder_ctx,
        )).await?;

        // Merge results
        let mut results = left_result.results;
        results.extend(right_result.results);
        return Ok(BatchNormalizeResult {
            results,
            model: left_result.model,
            prompt_tokens: left_result.prompt_tokens + right_result.prompt_tokens,
            completion_tokens: left_result.completion_tokens + right_result.completion_tokens,
            duration_ms: left_result.duration_ms + right_result.duration_ms,
        });
    }

    // Build and send
    let user_message = build_batch_user_message(
        &files, similar_artists, similar_releases, folder_ctx,
    );

    let messages = vec![
        ChatMessage { role: "system".into(), content: system_prompt.to_owned() },
        ChatMessage { role: "user".into(), content: user_message },
    ];

    let start = std::time::Instant::now();
    let call_result = call_llm_chat(
        llm_url, llm_model, &messages,
        if llm_auth.is_empty() { None } else { Some(llm_auth) },
    ).await;
    let duration_ms = start.elapsed().as_millis() as u64;

    // If LLM error and batch > 1, try splitting (handles context overflow errors)
    let (response_text, resp_model, usage) = match call_result {
        Ok(r) => r,
        Err(e) if files.len() > 1 => {
            let err_str = e.to_string().to_lowercase();
            let is_context_error = err_str.contains("context")
                || err_str.contains("too long")
                || err_str.contains("maximum")
                || err_str.contains("length")
                || err_str.contains("token");
            if is_context_error {
                tracing::warn!(
                    file_count = files.len(),
                    "LLM error suggests context overflow, splitting batch: {e}"
                );
                let mid = files.len() / 2;
                let mut files_vec = files;
                let right = files_vec.split_off(mid);
                let left = files_vec;

                let left_result = Box::pin(normalize_batch(
                    llm_url, llm_model, llm_auth, system_prompt, context_limit,
                    left, similar_artists, similar_releases, folder_ctx,
                )).await?;
                let right_result = Box::pin(normalize_batch(
                    llm_url, llm_model, llm_auth, system_prompt, context_limit,
                    right, similar_artists, similar_releases, folder_ctx,
                )).await?;

                let mut results = left_result.results;
                results.extend(right_result.results);
                return Ok(BatchNormalizeResult {
                    results,
                    model: left_result.model,
                    prompt_tokens: left_result.prompt_tokens + right_result.prompt_tokens,
                    completion_tokens: left_result.completion_tokens + right_result.completion_tokens,
                    duration_ms: left_result.duration_ms + right_result.duration_ms,
                });
            }
            return Err(e);
        }
        Err(e) => return Err(e),
    };

    let prompt_tokens = usage.prompt_tokens.unwrap_or(0) as u64;
    let completion_tokens = usage.completion_tokens.unwrap_or(0) as u64;

    // Parse batch response
    let results = parse_batch_response(&response_text, &files)?;

    Ok(BatchNormalizeResult {
        results,
        model: resp_model,
        prompt_tokens,
        completion_tokens,
        duration_ms,
    })
}

/// Parse a batch JSON array response from the LLM.
/// Returns (filename, NormalizedFields) pairs.
/// Handles: clean JSON array, markdown-fenced JSON, and wrapped `{"results": [...]}`.
fn parse_batch_response(
    response: &str,
    files: &[BatchFileInput],
) -> anyhow::Result<Vec<(String, NormalizedFields)>> {
    let cleaned = response.trim();

    // Strip markdown code fences if present
    let json_str = if cleaned.starts_with("```") {
        let start = cleaned.find('[')
            .or_else(|| cleaned.find('{'))
            .unwrap_or(0);
        let end_bracket = cleaned.rfind(']').map(|i| i + 1);
        let end_brace = cleaned.rfind('}').map(|i| i + 1);
        let end = end_bracket.or(end_brace).unwrap_or(cleaned.len());
        &cleaned[start..end]
    } else {
        cleaned
    };

    #[derive(Deserialize)]
    struct BatchLlmOutput {
        filename: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        title: Option<String>,
        year: Option<i32>,
        track_number: Option<i32>,
        genre: Option<String>,
        #[serde(default)]
        featured_artists: Vec<String>,
        release_type: Option<String>,
        confidence: Option<f64>,
        notes: Option<String>,
    }

    // Try parsing as array first, then as {"results": [...]} wrapper
    let items: Vec<BatchLlmOutput> = if json_str.starts_with('[') {
        serde_json::from_str(json_str)
    } else {
        // Try as wrapper object with a "results" or "files" key
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(alias = "files")]
            results: Vec<BatchLlmOutput>,
        }
        serde_json::from_str::<Wrapper>(json_str).map(|w| w.results)
    }
    .map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse batch LLM response: {} — raw: {}",
            e,
            &response[..response.len().min(500)]
        )
    })?;

    // Build a map of filename → NormalizedFields
    let mut results = Vec::with_capacity(files.len());
    let mut matched = std::collections::HashSet::new();

    for item in &items {
        let filename = match &item.filename {
            Some(f) => f.clone(),
            None => continue,
        };
        let fields = NormalizedFields {
            title: item.title.clone(),
            artist: item.artist.clone(),
            album: item.album.clone(),
            year: item.year,
            track_number: item.track_number,
            genre: item.genre.clone(),
            featured_artists: item.featured_artists.clone(),
            release_type: item.release_type.clone(),
            confidence: item.confidence,
            notes: item.notes.clone(),
        };
        matched.insert(filename.clone());
        results.push((filename, fields));
    }

    // Warn about files the LLM missed
    for f in files {
        if !matched.contains(&f.filename) {
            tracing::warn!(
                filename = %f.filename,
                "LLM batch response missing result for file"
            );
        }
    }

    Ok(results)
}
