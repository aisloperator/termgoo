//! Streaming decoder for DEC Sixel graphics.

pub const MAX_DIMENSION: usize = 4096;
pub const REGISTERS: usize = 256;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Data,
    /// Collecting numeric parameters for a command introduced by this byte.
    Params(u8),
}

pub struct Sixel {
    palette: [[u8; 3]; REGISTERS],
    color: usize,
    x: usize,
    /// Top of the current 6-pixel band.
    y: usize,
    state: State,
    params: Vec<u32>,
    repeat: usize,
    /// Pixel storage, row-major RGBA; zero alpha means "never drawn".
    pixels: Vec<u8>,
    alloc_w: usize,
    alloc_h: usize,
    width: usize,
    height: usize,
}

/// The VT340's power-on colour registers (values in percent).
const VT340: [[u8; 3]; 16] = [
    [0, 0, 0], [20, 20, 80], [80, 13, 13], [20, 80, 20],
    [80, 20, 80], [20, 80, 80], [80, 80, 20], [53, 53, 53],
    [26, 26, 26], [33, 33, 60], [60, 26, 26], [33, 60, 33],
    [60, 33, 60], [33, 60, 60], [60, 60, 33], [80, 80, 80],
];

fn percent(v: u32) -> u8 {
    (v.min(100) * 255 / 100) as u8
}

/// Sixel HLS has blue at hue 0, so it is rotated relative to the usual wheel.
fn hls(h: u32, l: u32, s: u32) -> [u8; 3] {
    let h = ((h + 240) % 360) as f64;
    let l = l.min(100) as f64 / 100.0;
    let s = s.min(100) as f64 / 100.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r, g, b].map(|v| ((v + m) * 255.0).round() as u8)
}

impl Sixel {
    pub fn new() -> Sixel {
        let mut palette = [[0u8; 3]; REGISTERS];
        for (i, p) in palette.iter_mut().enumerate() {
            *p = VT340[i % 16].map(|v| percent(v as u32));
        }
        Sixel {
            palette,
            color: 0,
            x: 0,
            y: 0,
            state: State::Data,
            params: Vec::new(),
            repeat: 1,
            pixels: Vec::new(),
            alloc_w: 0,
            alloc_h: 0,
            width: 0,
            height: 0,
        }
    }

    pub fn feed(&mut self, b: u8) {
        if let State::Params(cmd) = self.state {
            match b {
                b'0'..=b'9' => {
                    if self.params.is_empty() {
                        self.params.push(0);
                    }
                    let last = self.params.last_mut().unwrap();
                    *last = last.saturating_mul(10).saturating_add((b - b'0') as u32);
                    return;
                }
                b';' => {
                    if self.params.is_empty() {
                        self.params.push(0);
                    }
                    if self.params.len() < 8 {
                        self.params.push(0);
                    }
                    return;
                }
                _ => {
                    self.finish_command(cmd);
                    self.state = State::Data;
                }
            }
        }
        match b {
            b'?'..=b'~' => {
                let bits = b - b'?';
                let n = std::mem::replace(&mut self.repeat, 1);
                self.draw(bits, n);
            }
            b'#' | b'!' | b'"' => {
                self.params.clear();
                self.state = State::Params(b);
            }
            b'$' => self.x = 0,
            b'-' => {
                self.x = 0;
                self.y += 6;
            }
            _ => {}
        }
    }

    fn finish_command(&mut self, cmd: u8) {
        let p = |i: usize| self.params.get(i).copied().unwrap_or(0);
        match cmd {
            b'!' => self.repeat = (p(0) as usize).clamp(1, MAX_DIMENSION),
            b'"' => {
                // Raster attributes: Pan;Pad;Ph;Pv declares the image size.
                let (w, h) = (p(2) as usize, p(3) as usize);
                if self.params.len() >= 4 && w <= MAX_DIMENSION && h <= MAX_DIMENSION {
                    self.grow(w, h);
                    self.width = self.width.max(w);
                    self.height = self.height.max(h);
                }
            }
            b'#' => {
                let reg = p(0) as usize % REGISTERS;
                match (self.params.len() >= 5, p(1)) {
                    (true, 1) => self.palette[reg] = hls(p(2), p(3), p(4)),
                    (true, 2) => self.palette[reg] = [percent(p(2)), percent(p(3)), percent(p(4))],
                    _ => {}
                }
                self.color = reg;
            }
            _ => {}
        }
    }

    fn grow(&mut self, w: usize, h: usize) {
        if w <= self.alloc_w && h <= self.alloc_h {
            return;
        }
        let new_w = w.max(self.alloc_w * 2).clamp(64, MAX_DIMENSION);
        let new_h = h.max(self.alloc_h * 2).clamp(64, MAX_DIMENSION);
        let mut new = vec![0u8; new_w * new_h * 4];
        for y in 0..self.alloc_h {
            let n = self.alloc_w * 4;
            new[y * new_w * 4..][..n].copy_from_slice(&self.pixels[y * self.alloc_w * 4..][..n]);
        }
        self.pixels = new;
        self.alloc_w = new_w;
        self.alloc_h = new_h;
    }

    fn draw(&mut self, bits: u8, n: usize) {
        let x_end = (self.x + n).min(MAX_DIMENSION);
        if self.y + 6 <= MAX_DIMENSION && x_end > self.x {
            if bits != 0 {
                self.grow(x_end, self.y + 6);
                let [r, g, b] = self.palette[self.color];
                for bit in 0..6 {
                    if bits & (1 << bit) != 0 {
                        let row = (self.y + bit) * self.alloc_w;
                        for px in self.pixels[(row + self.x) * 4..(row + x_end) * 4].chunks_exact_mut(4) {
                            px.copy_from_slice(&[r, g, b, 255]);
                        }
                    }
                }
                let top = 8 - bits.leading_zeros() as usize;
                self.height = self.height.max(self.y + top);
            }
            self.width = self.width.max(x_end);
        }
        self.x = x_end;
    }

    /// Finish decoding, returning (width, height, RGBA) unless the image is empty.
    pub fn finish(mut self) -> Option<(u32, u32, Vec<u8>)> {
        if let State::Params(cmd) = self.state {
            self.finish_command(cmd);
        }
        if self.width == 0 || self.height == 0 {
            return None;
        }
        self.grow(self.width, self.height);
        let mut out = Vec::with_capacity(self.width * self.height * 4);
        for y in 0..self.height {
            out.extend_from_slice(&self.pixels[y * self.alloc_w * 4..][..self.width * 4]);
        }
        Some((self.width as u32, self.height as u32, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(s: &str) -> Option<(u32, u32, Vec<u8>)> {
        let mut d = Sixel::new();
        s.bytes().for_each(|b| d.feed(b));
        d.finish()
    }

    #[test]
    fn single_column_with_rgb_register() {
        let (w, h, px) = decode("#1;2;100;0;0#1~").unwrap();
        assert_eq!((w, h), (1, 6));
        assert_eq!(&px[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn repeat_and_newline() {
        let (w, h, _) = decode("#2!5~-!3~").unwrap();
        assert_eq!((w, h), (5, 12));
    }

    #[test]
    fn height_only_counts_drawn_bits() {
        let (_, h, _) = decode("A").unwrap(); // bits 000010: second pixel
        assert_eq!(h, 2);
    }

    #[test]
    fn raster_attributes_set_minimum_size() {
        let (w, h, px) = decode("\"1;1;10;20#1~").unwrap();
        assert_eq!((w, h), (10, 20));
        assert_eq!(px.len(), 10 * 20 * 4);
    }

    #[test]
    fn hls_primary_colors() {
        assert_eq!(hls(0, 50, 100), [0, 0, 255]);
        assert_eq!(hls(120, 50, 100), [255, 0, 0]);
        assert_eq!(hls(240, 50, 100), [0, 255, 0]);
    }

    #[test]
    fn huge_requests_are_bounded() {
        let (w, _, _) = decode("!999999~").unwrap();
        assert_eq!(w as usize, MAX_DIMENSION);
        assert!(decode("\"1;1;999999;999999").is_none());
    }
}
