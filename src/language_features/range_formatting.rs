use crate::capabilities::{attempt_server_capability, CAPABILITY_RANGE_FORMATTING};
use crate::context::*;
use crate::text_edit::{apply_text_edits_to_buffer, TextEditish};
use crate::types::*;
use lsp_types::request::*;
use lsp_types::*;
use serde::Deserialize;
use url::Url;

pub fn text_document_range_formatting(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let eligible_servers: Vec<_> = ctx
        .language_servers
        .iter()
        .filter(|srv| attempt_server_capability(*srv, &meta, CAPABILITY_RANGE_FORMATTING))
        .collect();
    if meta.fifo.is_none() && eligible_servers.is_empty() {
        return;
    }

    let params = RangeFormattingParams::deserialize(params)
        .expect("Params should follow RangeFormattingParams structure");

    let req_params = eligible_servers
        .into_iter()
        .map(|(server_name, _)| {
            (
                server_name.clone(),
                params
                    .ranges
                    .iter()
                    .map(|range| DocumentRangeFormattingParams {
                        text_document: TextDocumentIdentifier {
                            uri: Url::from_file_path(&meta.buffile).unwrap(),
                        },
                        range: *range,
                        options: params.formatting_options.clone(),
                        work_done_progress_params: Default::default(),
                    })
                    .collect(),
            )
        })
        .collect();
    ctx.call::<RangeFormatting, _>(
        meta,
        RequestParams::Each(req_params),
        move |ctx, meta, results| {
            let mut server = None;
            // First non-empty batches (all from the same server).
            let results: Vec<_> = results
                .into_iter()
                .filter(|(id, v)| match &server {
                    Some(chosen) => id == chosen && v.is_some(),
                    None if v.is_some() => {
                        server = Some(id.clone());
                        true
                    }
                    _ => false,
                })
                .collect();
            if let Some(first_id) = server {
                let text_edits = results
                    .into_iter()
                    .flat_map(|(_, v)| v.clone())
                    .flatten()
                    .collect::<Vec<_>>();
                editor_range_formatting(meta, (first_id, text_edits), ctx)
            }
        },
    );
}

pub fn editor_range_formatting<T: TextEditish<T>>(
    meta: EditorMeta,
    result: (ServerName, Vec<T>),
    ctx: &mut Context,
) {
    let (server_name, text_edits) = result;
    let server = &ctx.language_servers[&server_name];
    let cmd = ctx.documents.get(&meta.buffile).and_then(|document| {
        apply_text_edits_to_buffer(
            &meta.client,
            None,
            text_edits,
            &document.text,
            server.offset_encoding,
            false,
        )
    });
    match cmd {
        Some(cmd) => ctx.exec(meta, cmd),
        // Nothing to do, but sending command back to the editor is required to handle case when
        // editor is blocked waiting for response via fifo.
        None => ctx.exec(meta, "nop"),
    }
}
