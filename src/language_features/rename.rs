use crate::context::*;
use crate::position::*;
use crate::types::*;

use lsp_types::request::*;
use lsp_types::*;
use serde::Deserialize;
use url::Url;

use super::super::workspace;

pub fn text_document_rename(meta: EditorMeta, params: EditorParams, ctx: &mut Context) {
    let params = TextDocumentRenameParams::deserialize(params).unwrap();
    let req_params = ctx
        .language_servers
        .iter()
        .map(|(language_id, srv_settings)| {
            (
                language_id.clone(),
                vec![RenameParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: Url::from_file_path(&meta.buffile).unwrap(),
                        },
                        position: get_lsp_position(
                            srv_settings,
                            &meta.buffile,
                            &params.position,
                            ctx,
                        )
                        .unwrap(),
                    },
                    new_name: params.new_name,
                    work_done_progress_params: Default::default(),
                }],
            )
        })
        .collect();
    ctx.call::<Rename, _>(
        meta,
        RequestParams::Each(req_params),
        move |ctx: &mut Context, meta, results| {
            if let Some(result) = results.into_iter().find(|(_, v)| v.is_some()) {
                editor_rename(meta, result, ctx)
            }
        },
    );
}

// TODO handle version, so change is not applied if buffer is modified (and need to show a warning)
fn editor_rename(meta: EditorMeta, result: (LanguageId, Option<WorkspaceEdit>), ctx: &mut Context) {
    let (language_id, result) = result;
    if result.is_none() {
        return;
    }
    let result = result.unwrap();
    let srv_settings = &ctx.language_servers[&language_id];
    workspace::apply_edit(meta, srv_settings, result, ctx);
}
