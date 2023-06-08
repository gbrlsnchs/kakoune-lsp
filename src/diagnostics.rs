use crate::context::*;
use crate::controller::write_response_to_fifo;
use crate::markup::escape_kakoune_markup;
use crate::position::*;
use crate::types::*;
use crate::util::*;
use itertools::EitherOrBoth;
use itertools::Itertools;
use jsonrpc_core::Params;
use lsp_types::*;
use std::collections::HashMap;
use std::fmt::Write as _;

pub fn publish_diagnostics(language_id: &LanguageId, params: Params, ctx: &mut Context) {
    let params: PublishDiagnosticsParams = params.parse().expect("Failed to parse params");
    let path = params.uri.to_file_path().unwrap();
    let buffile = path.to_str().unwrap();
    ctx.diagnostics.insert(
        buffile.to_string(),
        params
            .diagnostics
            .into_iter()
            .map(|d| (language_id.clone(), d))
            .collect(),
    );
    let document = ctx.documents.get(buffile);
    if document.is_none() {
        return;
    }
    let document = document.unwrap();
    let version = document.version;
    let diagnostics = &ctx.diagnostics[buffile];
    let inline_diagnostics = diagnostics
        .iter()
        .sorted_unstable_by_key(|(_, x)| x.severity)
        .rev()
        .map(|(language_id, x)| {
            let server = &ctx.language_servers[language_id];
            format!(
                "{}|{}",
                lsp_range_to_kakoune(&x.range, &document.text, server.offset_encoding),
                match x.severity {
                    Some(DiagnosticSeverity::ERROR) => "DiagnosticError",
                    Some(DiagnosticSeverity::HINT) => "DiagnosticHint",
                    Some(DiagnosticSeverity::INFORMATION) => "DiagnosticInfo",
                    Some(DiagnosticSeverity::WARNING) | None => "DiagnosticWarning",
                    Some(_) => {
                        warn!("Unexpected DiagnosticSeverity: {:?}", x.severity);
                        "DiagnosticWarning"
                    }
                }
            )
        })
        .join(" ");

    // Assemble a list of diagnostics by line number
    let mut lines_with_diagnostics = HashMap::new();
    for (language_id, diagnostic) in diagnostics {
        let face = match diagnostic.severity {
            Some(DiagnosticSeverity::ERROR) => "InlayDiagnosticError",
            Some(DiagnosticSeverity::HINT) => "InlayDiagnosticHint",
            Some(DiagnosticSeverity::INFORMATION) => "InlayDiagnosticInfo",
            Some(DiagnosticSeverity::WARNING) | None => "InlayDiagnosticWarning",
            Some(_) => {
                warn!("Unexpected DiagnosticSeverity: {:?}", diagnostic.severity);
                "InlayDiagnosticWarning"
            }
        };
        let (_, line_diagnostics) = lines_with_diagnostics
            .entry(diagnostic.range.end.line)
            .or_insert((
                language_id.clone(),
                LineDiagnostics {
                    range_end: diagnostic.range.end,
                    symbols: String::new(),
                    text: "",
                    text_face: "",
                    text_severity: None,
                },
            ));

        let severity = diagnostic.severity.unwrap_or(DiagnosticSeverity::WARNING);
        if line_diagnostics
            .text_severity
            // Smaller == higher severity
            .map_or(true, |text_severity| severity < text_severity)
        {
            let first_line = diagnostic.message.split('\n').next().unwrap_or_default();
            line_diagnostics.text = first_line;
            line_diagnostics.text_face = face;
            line_diagnostics.text_severity = diagnostic.severity;
        }

        let _ = write!(
            line_diagnostics.symbols,
            "{{{}}}%opt[lsp_inlay_diagnostic_sign]",
            face
        );
    }

    // Assemble ranges based on the lines
    let inlay_diagnostics = lines_with_diagnostics
        .iter()
        .map(|(line_number, (language_id, line_diagnostics))| {
            let srv_settings = &ctx.language_servers[language_id];
            let line_text = get_line(*line_number as usize, &document.text);
            let mut pos = lsp_position_to_kakoune(
                &line_diagnostics.range_end,
                &document.text,
                srv_settings.offset_encoding,
            );
            pos.column = std::cmp::max(line_text.len_bytes() as u32, 1);

            format!(
                "\"{}+0|%opt[lsp_inlay_diagnostic_gap]{} {{{}}}{}\"",
                pos,
                line_diagnostics.symbols,
                line_diagnostics.text_face,
                editor_escape_double_quotes(&escape_tuple_element(&escape_kakoune_markup(
                    line_diagnostics.text
                )))
            )
        })
        .join(" ");

    let (line_flags, error_count, hint_count, info_count, warning_count) =
        gather_line_flags(ctx, buffile);

    // Always show a space on line one if no other highlighter is there,
    // to make sure the column always has the right width
    // Also wrap line_flags in another eval and quotes, to make sure the %opt[] tags are expanded
    let command = format!(
        "set-option buffer lsp_diagnostic_error_count {error_count}; \
         set-option buffer lsp_diagnostic_hint_count {hint_count}; \
         set-option buffer lsp_diagnostic_info_count {info_count}; \
         set-option buffer lsp_diagnostic_warning_count {warning_count}; \
         set-option buffer lsp_inline_diagnostics {version} {inline_diagnostics}; \
         evaluate-commands \"set-option buffer lsp_diagnostic_lines {version} {line_flags} '0|%opt[lsp_diagnostic_line_error_sign]'\"; \
         set-option buffer lsp_inlay_diagnostics {version} {inlay_diagnostics}"
    );
    let command = format!(
        "evaluate-commands -buffer {} %§{}§",
        editor_quote(buffile),
        command.replace('§', "§§")
    );
    let meta = ctx.meta_for_buffer_version(None, buffile, version);
    ctx.exec(meta, command);
}

pub fn gather_line_flags(ctx: &Context, buffile: &str) -> (String, u32, u32, u32, u32) {
    let diagnostics = ctx.diagnostics.get(buffile);
    let mut error_count: u32 = 0;
    let mut warning_count: u32 = 0;
    let mut info_count: u32 = 0;
    let mut hint_count: u32 = 0;

    let empty = vec![];
    let lenses = ctx
        .code_lenses
        .get(buffile)
        .unwrap_or(&empty)
        .iter()
        .map(|(_, lens)| (lens.range.start.line, "%opt[lsp_code_lens_sign]"));

    let empty = vec![];
    let diagnostics = diagnostics.unwrap_or(&empty).iter().map(|(_, x)| {
        (
            x.range.start.line,
            match x.severity {
                Some(DiagnosticSeverity::ERROR) => {
                    error_count += 1;
                    "{LineFlagError}%opt[lsp_diagnostic_line_error_sign]"
                }
                Some(DiagnosticSeverity::HINT) => {
                    hint_count += 1;
                    "{LineFlagHint}%opt[lsp_diagnostic_line_hint_sign]"
                }
                Some(DiagnosticSeverity::INFORMATION) => {
                    info_count += 1;
                    "{LineFlagInfo}%opt[lsp_diagnostic_line_info_sign]"
                }
                Some(DiagnosticSeverity::WARNING) | None => {
                    warning_count += 1;
                    "{LineFlagWarning}%opt[lsp_diagnostic_line_warning_sign]"
                }
                Some(_) => {
                    warn!("Unexpected DiagnosticSeverity: {:?}", x.severity);
                    ""
                }
            },
        )
    });

    let line_flags = diagnostics
        .merge_join_by(lenses, |left, right| left.0.cmp(&right.0))
        .map(|r| match r {
            EitherOrBoth::Left((line, diagnostic_label)) => (line, diagnostic_label),
            EitherOrBoth::Right((line, lens_label)) => (line, lens_label),
            EitherOrBoth::Both((line, diagnostic_label), _) => (line, diagnostic_label),
        })
        .map(|(line, label)| format!("'{}|{}'", line + 1, label))
        .join(" ");

    (
        line_flags,
        error_count,
        hint_count,
        info_count,
        warning_count,
    )
}

pub fn editor_diagnostics(meta: EditorMeta, ctx: &mut Context) {
    if meta.write_response_to_fifo {
        write_response_to_fifo(meta, &ctx.diagnostics);
        return;
    }
    let (_, main_settings) = ctx.language_servers.first_key_value().unwrap();
    let content = ctx
        .diagnostics
        .iter()
        .flat_map(|(filename, diagnostics)| {
            diagnostics
                .iter()
                .map(|(language_id, x)| {
                    let srv_settings = &ctx.language_servers[language_id];
                    let p = match get_kakoune_position(srv_settings, filename, &x.range.start, ctx)
                    {
                        Some(position) => position,
                        None => {
                            warn!("Cannot get position from file {}", filename);
                            return "".to_string();
                        }
                    };
                    format!(
                        "{}:{}:{}: {}: {}{}",
                        short_file_path(filename, &srv_settings.root_path),
                        p.line,
                        p.column,
                        match x.severity {
                            Some(DiagnosticSeverity::ERROR) => "error",
                            Some(DiagnosticSeverity::HINT) => "hint",
                            Some(DiagnosticSeverity::INFORMATION) => "info",
                            Some(DiagnosticSeverity::WARNING) | None => "warning",
                            Some(_) => {
                                warn!("Unexpected DiagnosticSeverity: {:?}", x.severity);
                                "warning"
                            }
                        },
                        x.message,
                        format_related_information(
                            x,
                            (language_id, srv_settings),
                            main_settings,
                            ctx
                        )
                        .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
        })
        .join("\n");
    let command = format!(
        "lsp-show-diagnostics {} {}",
        editor_quote(&main_settings.root_path),
        editor_quote(&content),
    );
    ctx.exec(meta, command);
}

pub fn format_related_information(
    d: &Diagnostic,
    srv: (&LanguageId, &ServerSettings),
    main_settings: &ServerSettings,
    ctx: &Context,
) -> Option<String> {
    let (language_id, srv_settings) = srv;
    d.related_information.as_ref().map(|infos| {
        "\n".to_string()
            + &infos
                .iter()
                .map(|info| {
                    let path = info.location.uri.to_file_path().unwrap();
                    let filename = path.to_str().unwrap();
                    let p = get_kakoune_position_with_fallback(
                        srv_settings,
                        filename,
                        info.location.range.start,
                        ctx,
                    );
                    format!(
                        "{}:{}:{}: ({language_id}) {}",
                        short_file_path(filename, &main_settings.root_path),
                        p.line,
                        p.column,
                        info.message
                    )
                })
                .join("\n")
    })
}
