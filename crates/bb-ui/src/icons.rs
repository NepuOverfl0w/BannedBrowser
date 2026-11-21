//! Original vector icons (16×16 design grid, line style, 1.5px strokes).

use bb_paint::{Path, Radii, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Back,
    Forward,
    Reload,
    Stop,
    Home,
    Menu,
    Close,
    Plus,
    Lock,
    LockBroken,
    Info,
    Star,
    Search,
    Globe,
    Mask,
    ChevronDown,
    Sidebar,
    Download,
}

const STROKE: f32 = 1.5;

/// Icon geometry scaled from the 16-unit grid to `size` at (x, y).
pub fn path(icon: Icon, x: f32, y: f32, size: f32) -> Path {
    let k = size / 16.0;
    let raw = raw_path(icon);
    raw.transform(k, k, x, y)
}

fn line(pts: &[(f32, f32)]) -> Path {
    let mut p = Path::new();
    for (i, (x, y)) in pts.iter().enumerate() {
        if i == 0 {
            p.move_to(*x, *y);
        } else {
            p.line_to(*x, *y);
        }
    }
    p.stroke(STROKE)
}

fn merge(paths: Vec<Path>) -> Path {
    let mut out = Path::new();
    for p in paths {
        out.cmds.extend(p.cmds);
    }
    out
}

/// Arc as a polyline (angles in radians, y-down).
fn arc(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
    let n = 24;
    (0..=n)
        .map(|i| {
            let t = a0 + (a1 - a0) * i as f32 / n as f32;
            (cx + r * t.cos(), cy + r * t.sin())
        })
        .collect()
}

fn raw_path(icon: Icon) -> Path {
    match icon {
        Icon::Back => {
            merge(vec![line(&[(13.0, 8.0), (3.0, 8.0)]), line(&[(7.5, 3.5), (3.0, 8.0), (7.5, 12.5)])])
        }
        Icon::Forward => {
            merge(vec![line(&[(3.0, 8.0), (13.0, 8.0)]), line(&[(8.5, 3.5), (13.0, 8.0), (8.5, 12.5)])])
        }
        Icon::Reload => {
            let pts = arc(8.0, 8.5, 5.0, -0.9, 4.6);
            let mut p = line(&pts);
            // arrow head at the start of the arc
            let (sx, sy) = pts[0];
            p.cmds.extend(
                Path::polygon(&[
                    bb_paint::Point::new(sx + 3.2, sy - 3.4),
                    bb_paint::Point::new(sx + 0.6, sy + 1.6),
                    bb_paint::Point::new(sx - 2.4, sy - 2.2),
                ])
                .cmds,
            );
            p
        }
        Icon::Stop | Icon::Close => {
            merge(vec![line(&[(4.0, 4.0), (12.0, 12.0)]), line(&[(12.0, 4.0), (4.0, 12.0)])])
        }
        Icon::Home => merge(vec![
            line(&[(2.0, 8.0), (8.0, 2.5), (14.0, 8.0)]),
            line(&[(4.0, 6.5), (4.0, 13.5), (12.0, 13.5), (12.0, 6.5)]),
        ]),
        Icon::Menu => merge(vec![
            line(&[(2.5, 4.0), (13.5, 4.0)]),
            line(&[(2.5, 8.0), (13.5, 8.0)]),
            line(&[(2.5, 12.0), (13.5, 12.0)]),
        ]),
        Icon::Plus => merge(vec![line(&[(8.0, 2.5), (8.0, 13.5)]), line(&[(2.5, 8.0), (13.5, 8.0)])]),
        Icon::Lock | Icon::LockBroken => {
            let mut body = Path::rounded_rect(Rect::new(3.0, 7.0, 10.0, 7.5), Radii::uniform(1.5));
            let shackle = if icon == Icon::Lock {
                let mut pts = vec![(5.25, 7.0)];
                pts.extend(arc(8.0, 5.0, 2.75, std::f32::consts::PI, std::f32::consts::TAU));
                pts.push((10.75, 7.0));
                line(&pts)
            } else {
                let mut pts = vec![(5.25, 7.0)];
                pts.extend(arc(8.0, 5.0, 2.75, std::f32::consts::PI, 1.8 * std::f32::consts::PI));
                line(&pts)
            };
            body.cmds.extend(shackle.cmds);
            body
        }
        Icon::Info => {
            let mut p = line(&arc(8.0, 8.0, 6.0, 0.0, std::f32::consts::TAU));
            p.cmds.extend(line(&[(8.0, 7.0), (8.0, 11.5)]).cmds);
            p.cmds.extend(Path::ellipse(8.0, 4.8, 1.0, 1.0).cmds);
            p
        }
        Icon::Star => {
            let mut pts = Vec::new();
            for i in 0..=10 {
                let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                let r = if i % 2 == 0 { 6.5 } else { 2.8 };
                pts.push((8.0 + r * a.cos(), 8.5 + r * a.sin()));
            }
            line(&pts)
        }
        Icon::Search => {
            let mut p = line(&arc(7.0, 7.0, 4.5, 0.0, std::f32::consts::TAU));
            p.cmds.extend(line(&[(10.3, 10.3), (14.0, 14.0)]).cmds);
            p
        }
        Icon::Globe => {
            let mut p = line(&arc(8.0, 8.0, 6.0, 0.0, std::f32::consts::TAU));
            p.cmds.extend(line(&[(2.0, 8.0), (14.0, 8.0)]).cmds);
            let mut e = Vec::new();
            for i in 0..=24 {
                let t = i as f32 / 24.0 * std::f32::consts::TAU;
                e.push((8.0 + 2.6 * t.cos(), 8.0 + 6.0 * t.sin()));
            }
            p.cmds.extend(line(&e).cmds);
            p
        }
        Icon::Mask => {
            // Private-browsing mask: two eye holes in a rounded band (original design).
            let mut p = Path::rounded_rect(Rect::new(1.0, 5.0, 14.0, 6.5), Radii::uniform(3.0));
            p.add_rounded_rect(Rect::new(3.0, 6.8, 3.6, 2.6), Radii::uniform(1.3), true);
            p.add_rounded_rect(Rect::new(9.4, 6.8, 3.6, 2.6), Radii::uniform(1.3), true);
            p
        }
        Icon::ChevronDown => line(&[(4.0, 6.0), (8.0, 10.0), (12.0, 6.0)]),
        Icon::Sidebar => {
            let mut p = line(&[(2.0, 3.0), (14.0, 3.0), (14.0, 13.0), (2.0, 13.0), (2.0, 3.0)]);
            p.cmds.extend(line(&[(6.0, 3.0), (6.0, 13.0)]).cmds);
            p
        }
        Icon::Download => merge(vec![
            line(&[(8.0, 2.5), (8.0, 10.5)]),
            line(&[(4.5, 7.0), (8.0, 10.5), (11.5, 7.0)]),
            line(&[(3.0, 13.5), (13.0, 13.5)]),
        ]),
    }
}

/// The BannedBrowser app icon: a pixel/glitch-block "B" (original artwork).
/// Returns filled cells on a 16×16 grid: (x, y, color index 0=violet, 1=cyan, 2=white).
pub fn logo_cells() -> Vec<(u8, u8, u8)> {
    const ART: [&str; 16] = [
        "................",
        "..VVVVVVVVV.....",
        "..VVVVVVVVVV....",
        "..VVW....VVVC...",
        "..VVW.....VVC...",
        "..VVW....VVVC...",
        "..VVVVVVVVVC....",
        "..VVVVVVVVVVC...",
        "..VVW.....VVVC..",
        "..VVW......VVC..",
        "C.VVW......VVC..",
        "..VVW.....VVVC..",
        "..VVVVVVVVVVC...",
        "..VVVVVVVVVC.C..",
        ".CC.........C...",
        "................",
    ];
    let mut out = Vec::new();
    for (y, row) in ART.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            let c = match ch {
                'V' => 0,
                'C' => 1,
                'W' => 2,
                _ => continue,
            };
            out.push((x as u8, y as u8, c));
        }
    }
    out
}
