//! Donate dialog — payment QR codes with cute copy.

use std::borrow::Cow;

use egui::{Color32, Context, CornerRadius, Stroke, Vec2};
use crate::app::QLogApp;

/// Try to load a QR PNG image and convert it to an egui texture.
/// Returns `None` if the image can't be decoded.
///
/// 赞赏码：优先读 sidecar (`<exe>/assets/<name>`)，找不到走编译期嵌入。
fn load_qr_image(ctx: &Context, filename: &str) -> Option<egui::TextureHandle> {
    let bytes: Cow<'static, [u8]> = match filename {
        "donate_wechat.png" => crate::assets::donate_wechat_png(),
        "donate_alipay.png" => crate::assets::donate_alipay_png(),
        _ => return None,
    };

    // Minimal PNG decoder — handles 8-bit RGB and RGBA PNGs.
    let img = decode_png_rgba(&bytes)?;

    let size = [img.width as usize, img.height as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &img.pixels);
    Some(ctx.load_texture(
        filename,
        color_image,
        egui::TextureOptions {
            magnification: egui::TextureFilter::Linear,
            minification: egui::TextureFilter::Linear,
            ..Default::default()
        },
    ))
}

/// A tiny, minimal PNG decoder that only handles the common case:
/// 8-bit RGB or RGBA, no interlacing, no palette.
struct DecodedImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>, // RGBA
}

fn decode_png_rgba(data: &[u8]) -> Option<DecodedImage> {
    let decoder = png::Decoder::new(data);
    let mut reader = decoder.read_info().ok()?;

    let info = reader.info();
    let width = info.width;
    let height = info.height;
    let color_type = info.color_type;

    // We only handle RGB and RGBA for simplicity.
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }

    let mut buf = vec![0u8; reader.output_buffer_size()];
    reader.next_frame(&mut buf).ok()?;

    let pixels = match color_type {
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity((width * height * 4) as usize);
            for chunk in buf.chunks(3) {
                rgba.push(chunk[0]);
                rgba.push(chunk[1]);
                rgba.push(chunk[2]);
                rgba.push(255);
            }
            rgba
        }
        png::ColorType::Rgba => buf,
        _ => return None,
    };

    Some(DecodedImage { width, height, pixels })
}

/// Render a QR code area — either the loaded image or a placeholder frame.
fn qr_area(ui: &mut egui::Ui, tex: &Option<egui::TextureHandle>, label: &str, hint: &str) {
    let size = 180.0;
    let rounding = CornerRadius::same(12);

    match tex {
        Some(t) => {
            // Show the actual QR image.
            let img = egui::Image::from_texture(egui::load::SizedTexture::new(t.id(), [size, size]));
            ui.add(img);
        }
        None => {
            // Placeholder: a rounded border frame with instructions.
            let (rect, _resp) = ui.allocate_exact_size(
                Vec2::new(size, size),
                egui::Sense::hover(),
            );

            // Background
            ui.painter().rect_filled(
                rect,
                rounding,
                Color32::from_gray(35),
            );
            // Border
            ui.painter().rect_stroke(
                rect,
                rounding,
                Stroke::new(1.5, Color32::from_gray(70)),
                egui::StrokeKind::Inside,
            );

            // Centered text inside the frame.
            let text_galley = ui.fonts(|f| {
                f.layout_no_wrap(
                    format!("{}\n\n{}", label, hint),
                    egui::FontId::proportional(12.0),
                    Color32::from_gray(140),
                )
            });
            let text_size = text_galley.rect.size();
            let text_pos = egui::pos2(
                rect.center().x - text_size.x * 0.5,
                rect.center().y - text_size.y * 0.5,
            );
            ui.painter().galley(text_pos, text_galley, Color32::WHITE);
        }
    }
}

pub fn render_donate(ctx: &Context, app: &mut QLogApp) {
    // Lazy-load QR images on first open.
    let wechat_tex: Option<egui::TextureHandle> = load_qr_image(ctx, "donate_wechat.png");
    let alipay_tex: Option<egui::TextureHandle> = load_qr_image(ctx, "donate_alipay.png");

    crate::dialogs::centered_window(ctx, "❤ 支持作者", [500.0, 420.0])
        .fixed_size([500.0, 420.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);

                // ── Title ──────────────────────────────────────────
                ui.label(
                    egui::RichText::new("❤ 为作者续命 ❤")
                        .size(22.0)
                        .strong()
                        .color(Color32::from_rgb(255, 140, 160)),
                );

                ui.add_space(6.0);

                ui.label(
                    egui::RichText::new("如果你觉得这个工具还不错，可以考虑请作者喝杯奶茶~")
                        .size(13.0)
                        .color(Color32::from_gray(185)),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("每一份支持都是作者熬夜修 bug 的动力 🔧")
                        .size(13.0)
                        .color(Color32::from_gray(170)),
                );

                ui.add_space(18.0);

                // ── QR codes side by side ──────────────────────────
                ui.horizontal(|ui| {
                    ui.add_space(30.0);

                    ui.vertical(|ui| {
                        qr_area(ui, &wechat_tex, "微信赞赏码", "请将 donate_wechat.png\n放入 assets/ 目录");
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("微信支付")
                                .size(13.0)
                                .color(Color32::from_rgb(140, 210, 120)),
                        );
                    });

                    ui.add_space(40.0);

                    ui.vertical(|ui| {
                        qr_area(ui, &alipay_tex, "支付宝收款码", "请将 donate_alipay.png\n放入 assets/ 目录");
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("支付宝")
                                .size(13.0)
                                .color(Color32::from_rgb(100, 170, 240)),
                        );
                    });

                    ui.add_space(30.0);
                });

                ui.add_space(14.0);

                // ── Cute footer ────────────────────────────────────
                ui.label(
                    egui::RichText::new("❤ 每一笔捐赠都在为作者的 API token 续命")
                        .size(12.0)
                        .color(Color32::from_gray(155)),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("☕ 足够买显卡的话说不定会加入 GPU 加速复杂搜索")
                        .size(12.0)
                        .color(Color32::from_gray(150)),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("🐛 当然也欢迎提交 Issue / PR 来修 bug")
                        .size(12.0)
                        .color(Color32::from_gray(155)),
                );

                ui.add_space(18.0);

                // ── Close button ───────────────────────────────────
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("先白嫖着，下次一定").color(Color32::WHITE).size(13.0),
                        )
                        .fill(Color32::from_rgb(100, 110, 125))
                        .min_size(egui::vec2(170.0, 30.0)),
                    )
                    .clicked()
                {
                    app.show_donate = false;
                }
            });
        });
}
