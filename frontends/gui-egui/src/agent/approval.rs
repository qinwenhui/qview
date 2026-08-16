//! ApprovalRequired 弹窗（架构 §9.3）。

use egui::{Align, Color32, Modal, RichText};

use qview_agent::handle::ProposalDecision;

use super::state::AgentPanelState;

/// 显示审批弹窗（如果在 pending_proposal 里有内容）。
pub fn show_modal(ctx: &egui::Context, state: &AgentPanelState, app: &crate::app::QLogApp) {
    let pending = state.pending_proposal.lock().clone();
    let Some((proposal_id, tool, reason)) = pending else {
        return;
    };

    let modal = Modal::new(egui::Id::new("agent-approval"));
    modal.show(ctx, |ui| {
        ui.set_width(420.0);
        ui.vertical(|ui| {
            ui.label(RichText::new("⚠ 器灵请求执行写操作").strong().size(16.0));
            ui.add_space(8.0);
            ui.label(format!("工具: {}", tool));
            ui.label(format!("原因: {}", reason));
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                if ui.button(RichText::new("✓ 通过").color(Color32::GREEN)).clicked() {
                    decide(state, app, proposal_id, ProposalDecision::Approve);
                }
                if ui.button(RichText::new("✗ 拒绝").color(Color32::RED)).clicked() {
                    decide(state, app, proposal_id, ProposalDecision::Reject);
                }
                if ui.button("忽略").clicked() {
                    decide(state, app, proposal_id, ProposalDecision::Skip);
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("关闭").clicked() {
                        *state.pending_proposal.lock() = None;
                    }
                });
            });
        });
    });
}

fn decide(
    state: &AgentPanelState,
    app: &crate::app::QLogApp,
    id: qview_application::protocol::ProposalId,
    decision: ProposalDecision,
) {
    *state.pending_proposal.lock() = None;
    if let Some(h) = state.handle.lock().clone() {
        let h2 = h.clone();
        app.spawn_tokio(async move {
            let _ = h2.proposal_decision(id, decision).await;
        });
    }
}
