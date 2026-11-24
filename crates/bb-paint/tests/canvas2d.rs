//! The 2D context's drawing model: pixels after fills, strokes, gradients, compositing,
//! clips, images and image data.

use bb_paint::canvas::Color;
use bb_paint::canvas2d::{Bitmap, Composite, Context2D, Gradient, GradientKind, LineCap, Matrix, Style};

fn px(c: &Context2D, x: i32, y: i32) -> [u8; 4] {
    let d = c.get_image_data(x, y, 1, 1);
    [d[0], d[1], d[2], d[3]]
}

fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| (*x as i32 - *y as i32).abs() <= 2)
}

fn red() -> Style {
    Style::Color(Color::rgba(255, 0, 0, 255))
}

#[test]
fn fill_rect_and_clear_rect() {
    let mut c = Context2D::new(100, 50);
    c.state.fill = red();
    c.fill_rect(10.0, 10.0, 20.0, 20.0);
    assert_eq!(px(&c, 15, 15), [255, 0, 0, 255]);
    assert_eq!(px(&c, 5, 5), [0, 0, 0, 0]);
    c.clear_rect(12.0, 12.0, 4.0, 4.0);
    assert_eq!(px(&c, 13, 13), [0, 0, 0, 0]);
    assert_eq!(px(&c, 20, 20), [255, 0, 0, 255]);
}

#[test]
fn transforms_paths_and_arcs() {
    let mut c = Context2D::new(100, 100);
    c.state.fill = Style::Color(Color::rgba(0, 128, 0, 255));
    c.state.transform = Matrix::new(1.0, 0.0, 0.0, 1.0, 50.0, 50.0);
    c.builder().ellipse(0.0, 0.0, 20.0, 20.0, 0.0, 0.0, std::f32::consts::TAU, false);
    c.fill(None, false);
    assert_eq!(px(&c, 50, 50), [0, 128, 0, 255]);
    assert_eq!(px(&c, 50, 25), [0, 0, 0, 0]);
    assert!(c.point_in_path(None, 60.0, 50.0, false));
    assert!(!c.point_in_path(None, 80.0, 50.0, false));
}

#[test]
fn evenodd_and_strokes() {
    let mut c = Context2D::new(100, 100);
    c.state.fill = red();
    let mut b = c.builder();
    b.rect(10.0, 10.0, 80.0, 80.0);
    b.rect(30.0, 30.0, 40.0, 40.0);
    c.fill(None, true);
    assert_eq!(px(&c, 50, 50), [0, 0, 0, 0], "the hole of an even-odd fill");
    assert_eq!(px(&c, 20, 20), [255, 0, 0, 255]);
    let mut s = Context2D::new(100, 100);
    s.state.stroke = Style::Color(Color::rgba(0, 0, 255, 255));
    s.state.line_width = 10.0;
    s.state.cap = LineCap::Square;
    let mut b = s.builder();
    b.move_to(20.0, 50.0);
    b.line_to(80.0, 50.0);
    s.stroke(None);
    assert_eq!(px(&s, 50, 47), [0, 0, 255, 255]);
    assert_eq!(px(&s, 17, 50), [0, 0, 255, 255], "square cap extends past the end");
    assert_eq!(px(&s, 50, 40), [0, 0, 0, 0]);
    assert!(s.point_in_stroke(None, 50.0, 53.0));
}

#[test]
fn gradients() {
    let mut c = Context2D::new(100, 10);
    let mut g =
        Gradient { kind: GradientKind::Linear { x0: 0.0, y0: 0.0, x1: 100.0, y1: 0.0 }, stops: Vec::new() };
    g.add_stop(0.0, Color::rgba(0, 0, 0, 255));
    g.add_stop(1.0, Color::rgba(255, 255, 255, 255));
    c.state.fill = Style::Gradient(std::sync::Arc::new(g));
    c.fill_rect(0.0, 0.0, 100.0, 10.0);
    let mid = px(&c, 50, 5);
    assert!((mid[0] as i32 - 128).abs() <= 3, "{mid:?}");
    let mut r = Context2D::new(100, 100);
    let mut g = Gradient {
        kind: GradientKind::Radial { x0: 50.0, y0: 50.0, r0: 0.0, x1: 50.0, y1: 50.0, r1: 50.0 },
        stops: Vec::new(),
    };
    g.add_stop(0.0, Color::rgba(255, 0, 0, 255));
    g.add_stop(1.0, Color::rgba(0, 0, 255, 255));
    r.state.fill = Style::Gradient(std::sync::Arc::new(g));
    r.fill_rect(0.0, 0.0, 100.0, 100.0);
    assert!(px(&r, 50, 50)[0] > 240);
    assert!(px(&r, 99, 50)[2] > 240);
}

#[test]
fn compositing_alpha_and_clip() {
    let mut c = Context2D::new(50, 50);
    c.state.fill = red();
    c.fill_rect(0.0, 0.0, 50.0, 50.0);
    c.state.composite = Composite::DestinationOut;
    c.state.fill = Style::Color(Color::rgba(0, 0, 0, 255));
    c.fill_rect(0.0, 0.0, 25.0, 50.0);
    assert_eq!(px(&c, 10, 10), [0, 0, 0, 0]);
    assert_eq!(px(&c, 40, 10), [255, 0, 0, 255]);
    // copy clears everything outside the shape.
    c.state.composite = Composite::Copy;
    c.state.fill = Style::Color(Color::rgba(0, 255, 0, 255));
    c.fill_rect(0.0, 0.0, 10.0, 10.0);
    assert_eq!(px(&c, 40, 10), [0, 0, 0, 0]);
    assert_eq!(px(&c, 5, 5), [0, 255, 0, 255]);
    // Global alpha and a clip.
    let mut d = Context2D::new(50, 50);
    d.builder().rect(0.0, 0.0, 20.0, 50.0);
    d.clip(None, false);
    d.state.alpha = 0.5;
    d.state.fill = red();
    d.fill_rect(0.0, 0.0, 50.0, 50.0);
    assert!(near(px(&d, 10, 10), [255, 0, 0, 128]), "{:?}", px(&d, 10, 10));
    assert_eq!(px(&d, 30, 10), [0, 0, 0, 0]);
}

#[test]
fn image_data_and_images() {
    let mut c = Context2D::new(4, 4);
    let data = [10u8, 20, 30, 255].repeat(4);
    c.put_image_data(&data, 2, 2, 1, 1, (0, 0, 2, 2));
    assert_eq!(px(&c, 1, 1), [10, 20, 30, 255]);
    assert_eq!(px(&c, 0, 0), [0, 0, 0, 0]);
    let mut img = Bitmap::new(2, 2);
    for p in img.data.chunks_mut(4) {
        p.copy_from_slice(&[0, 0, 255, 255]);
    }
    let mut d = Context2D::new(20, 20);
    d.state.smoothing = false;
    d.draw_image(&img, 0.0, 0.0, 2.0, 2.0, 5.0, 5.0, 10.0, 10.0);
    assert_eq!(px(&d, 10, 10), [0, 0, 255, 255]);
    assert_eq!(px(&d, 2, 2), [0, 0, 0, 0]);
}

#[test]
fn shadows() {
    let mut c = Context2D::new(60, 60);
    c.state.fill = red();
    c.state.shadow_color = Color::rgba(0, 0, 255, 255);
    c.state.shadow_x = 20.0;
    c.state.shadow_y = 20.0;
    c.fill_rect(5.0, 5.0, 20.0, 20.0);
    assert_eq!(px(&c, 10, 10), [255, 0, 0, 255]);
    assert_eq!(px(&c, 40, 40), [0, 0, 255, 255]);
}

#[test]
fn fonts_parse() {
    let f =
        bb_paint::canvas2d::parse_font("italic bold 20px/30px Arial, \"Times New Roman\", serif").unwrap();
    assert!(f.italic);
    assert_eq!(f.weight, 700);
    assert_eq!(f.size, 20.0);
    assert_eq!(f.families, ["Arial", "Times New Roman", "serif"]);
    assert!(bb_paint::canvas2d::parse_font("bogus").is_none());
}
