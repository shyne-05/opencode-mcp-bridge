use crate::{
    process::{run_program, spawn_detached},
    state::AppState,
};

pub async fn open_app(state: &AppState, app: &str) -> Result<String, String> {
    let app = app.trim();
    if app.is_empty() {
        return Err("app is required".to_string());
    }
    if app.contains('\0') || app.contains('\n') || app.contains('\r') {
        return Err("app contains invalid characters".to_string());
    }

    if let Some(flatpak_id) = find_flatpak_app(state, app).await? {
        return spawn_detached(
            "flatpak",
            &["run".to_string(), flatpak_id.clone()],
            None,
            &state.config.process,
        )
        .await
        .map(|result| format!("{result}\nflatpak:{flatpak_id}"));
    }

    if let Ok(result) = spawn_detached(
        "gtk-launch",
        &[app.to_string()],
        None,
        &state.config.process,
    )
    .await
    {
        return Ok(result);
    }

    spawn_detached(app, &[], None, &state.config.process).await
}

async fn find_flatpak_app(state: &AppState, app: &str) -> Result<Option<String>, String> {
    let output = run_program(
        "flatpak",
        &[
            "list".to_string(),
            "--app".to_string(),
            "--columns=application,name".to_string(),
        ],
        None,
        state.config.process.browser_timeout,
        &state.config.process,
    )
    .await;
    if output.code != Some(0) || output.timed_out {
        return Ok(None);
    }
    select_flatpak_app(&output.stdout, app)
}

fn select_flatpak_app(list: &str, app: &str) -> Result<Option<String>, String> {
    let needle = app.to_ascii_lowercase();
    let mut partial = Vec::new();
    for line in list.lines() {
        let mut columns = line.splitn(2, '\t');
        let id = columns.next().unwrap_or_default().trim();
        if id.is_empty() {
            continue;
        }
        let name = columns.next().unwrap_or_default().trim();
        if id.eq_ignore_ascii_case(app) || name.eq_ignore_ascii_case(app) {
            return Ok(Some(id.to_string()));
        }
        if id.to_ascii_lowercase().contains(&needle) || name.to_ascii_lowercase().contains(&needle)
        {
            partial.push(id.to_string());
        }
    }
    partial.sort();
    partial.dedup();
    match partial.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        _ => Err(format!(
            "application name is ambiguous; matching Flatpak IDs: {}",
            partial
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub async fn audio_status(state: &AppState) -> Result<String, String> {
    let output = run_program(
        "wpctl",
        &["get-volume".to_string(), "@DEFAULT_AUDIO_SINK@".to_string()],
        None,
        state.config.process.browser_timeout,
        &state.config.process,
    )
    .await;
    if output.code == Some(0) && !output.timed_out {
        Ok(output.stdout.trim().to_string())
    } else {
        Err(output.render())
    }
}

pub async fn set_volume(state: &AppState, volume: u8, unmute: bool) -> Result<String, String> {
    if volume > 100 {
        return Err("volume must be between 0 and 100".to_string());
    }
    let set = run_program(
        "wpctl",
        &[
            "set-volume".to_string(),
            "@DEFAULT_AUDIO_SINK@".to_string(),
            format!("{volume}%"),
        ],
        None,
        state.config.process.browser_timeout,
        &state.config.process,
    )
    .await;
    if set.code != Some(0) || set.timed_out {
        return Err(set.render());
    }
    if unmute {
        let unmute_result = run_program(
            "wpctl",
            &[
                "set-mute".to_string(),
                "@DEFAULT_AUDIO_SINK@".to_string(),
                "0".to_string(),
            ],
            None,
            state.config.process.browser_timeout,
            &state.config.process,
        )
        .await;
        if unmute_result.code != Some(0) || unmute_result.timed_out {
            return Err(unmute_result.render());
        }
    }
    audio_status(state).await
}

#[cfg(test)]
mod tests {
    use super::select_flatpak_app;

    #[test]
    fn flatpak_selection_prefers_exact_and_rejects_ambiguity() {
        let list = "com.spotify.Client\tSpotify\norg.foo.Music\tMusic Player\norg.bar.Music\tMusic Studio\n";
        assert_eq!(
            select_flatpak_app(list, "Spotify").unwrap().as_deref(),
            Some("com.spotify.Client")
        );
        assert_eq!(
            select_flatpak_app(list, "spotify").unwrap().as_deref(),
            Some("com.spotify.Client")
        );
        assert!(select_flatpak_app(list, "Music").is_err());
        assert_eq!(select_flatpak_app(list, "Missing").unwrap(), None);
    }
}
