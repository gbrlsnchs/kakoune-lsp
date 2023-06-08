use crate::capabilities::{attempt_server_capability, CAPABILITY_FORMATTING};
use crate::context::*;
use crate::types::*;
use lsp_types::request::*;
use lsp_types::*;
use serde::Deserialize;
use url::Url;

pub fn text_document_formatting(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let eligible_servers: Vec<_> = ctx
        .language_servers
        .iter()
        .filter(|srv| attempt_server_capability(*srv, &meta, CAPABILITY_FORMATTING))
        .collect();
    if meta.fifo.is_none() && eligible_servers.is_empty() {
        return;
    }

    let params = FormattingOptions::deserialize(params)
        .expect("Params should follow FormattingOptions structure");
    let req_params = eligible_servers
        .into_iter()
        .map(|(language_id, _)| {
            (
                language_id.clone(),
                vec![DocumentFormattingParams {
                    text_document: TextDocumentIdentifier {
                        uri: Url::from_file_path(&meta.buffile).unwrap(),
                    },
                    options: params,
                    work_done_progress_params: Default::default(),
                }],
            )
        })
        .collect();
    ctx.call::<Formatting, _>(
        meta,
        RequestParams::Each(req_params),
        move |ctx, meta, results| {
            if let Some((language_id, result)) = results.into_iter().find(|(_, v)| v.is_some()) {
                let text_edits = result.unwrap_or_default();
                super::range_formatting::editor_range_formatting(
                    meta,
                    (language_id, text_edits),
                    ctx,
                )
            }
        },
    );
}
