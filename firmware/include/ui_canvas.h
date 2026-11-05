#pragma once
#include <Arduino.h>
#include <math.h>
#include <vector>
#include "ui_theme.h"

constexpr int WIDTH = 1024, HEIGHT = 600;

// Software renderer over an RGB565 framebuffer with anti-aliased primitives.
// Every primitive honours the clip rectangle (used for scrolling regions).
class Canvas {
public:
  uint16_t *pixels = nullptr;
  int cx0 = 0, cy0 = 0, cx1 = WIDTH, cy1 = HEIGHT;

  void clip(int x, int y, int w, int h) { cx0 = max(0, x); cy0 = max(0, y); cx1 = min(WIDTH, x + w); cy1 = min(HEIGHT, y + h); }
  void unclip() { cx0 = 0; cy0 = 0; cx1 = WIDTH; cy1 = HEIGHT; }

  void blend(int x, int y, uint16_t color, uint8_t alpha) {
    if (!alpha || x < cx0 || x >= cx1 || y < cy0 || y >= cy1) return;
    uint16_t &dst = pixels[y * WIDTH + x];
    if (alpha == 255) { dst = color; return; }
    int inv = 255 - alpha;
    int r = (((color >> 11) & 31) * alpha + ((dst >> 11) & 31) * inv + 127) / 255;
    int g = (((color >> 5) & 63) * alpha + ((dst >> 5) & 63) * inv + 127) / 255;
    int b = ((color & 31) * alpha + (dst & 31) * inv + 127) / 255;
    dst = (r << 11) | (g << 5) | b;
  }
  void fillRect(int x, int y, int w, int h, uint16_t color) {
    int x0 = max(cx0, x), y0 = max(cy0, y), x1 = min(cx1, x + w), y1 = min(cy1, y + h);
    for (int row = y0; row < y1; row++) {
      uint16_t *p = pixels + row * WIDTH + x0;
      for (int col = x0; col < x1; col++) *p++ = color;
    }
  }
  void fillScreen(uint16_t color) { fillRect(0, 0, WIDTH, HEIGHT, color); }
  static uint8_t coverage(float c) { return c <= 0 ? 0 : c >= 1 ? 255 : uint8_t(c * 255); }

  void fillRoundRect(int x, int y, int w, int h, int r, uint16_t color) {
    if (w <= 0 || h <= 0) return;
    r = min(r, min(w, h) / 2);
    if (r <= 0) { fillRect(x, y, w, h, color); return; }
    fillRect(x, y + r, w, h - 2 * r, color);
    fillRect(x + r, y, w - 2 * r, r, color);
    fillRect(x + r, y + h - r, w - 2 * r, r, color);
    for (int dy = 0; dy < r; dy++) for (int dx = 0; dx < r; dx++) {
      float fx = r - dx - 0.5f, fy = r - dy - 0.5f;
      uint8_t a = coverage(r - sqrtf(fx * fx + fy * fy) + 0.5f);
      if (!a) continue;
      blend(x + dx, y + dy, color, a); blend(x + w - 1 - dx, y + dy, color, a);
      blend(x + dx, y + h - 1 - dy, color, a); blend(x + w - 1 - dx, y + h - 1 - dy, color, a);
    }
  }
  void fillCircle(float cx, float cy, float r, uint16_t color, uint8_t alpha = 255) {
    for (int y = int(cy - r - 1); y <= int(cy + r + 1); y++) for (int x = int(cx - r - 1); x <= int(cx + r + 1); x++) {
      float dx = x + 0.5f - cx, dy = y + 0.5f - cy;
      uint8_t a = coverage(r - sqrtf(dx * dx + dy * dy) + 0.5f);
      if (a) blend(x, y, color, uint16_t(a) * alpha / 255);
    }
  }
  // Arc of a ring; angles in degrees, 0 = right, clockwise (screen y grows down).
  void arc(float cx, float cy, float r, float thick, float startDeg, float sweepDeg, uint16_t color) {
    float inner = r - thick;
    for (int y = int(cy - r - 1); y <= int(cy + r + 1); y++) for (int x = int(cx - r - 1); x <= int(cx + r + 1); x++) {
      float dx = x + 0.5f - cx, dy = y + 0.5f - cy, d = sqrtf(dx * dx + dy * dy);
      uint8_t a = coverage(min(r - d + 0.5f, d - inner + 0.5f));
      if (!a) continue;
      if (sweepDeg < 360) {
        float ang = atan2f(dy, dx) * 57.29578f - startDeg;
        while (ang < 0) ang += 360;
        while (ang >= 360) ang -= 360;
        if (ang > sweepDeg) continue;
      }
      blend(x, y, color, a);
    }
  }
  // Anti-aliased thick line with round caps.
  void stroke(float x0, float y0, float x1, float y1, float width, uint16_t color) {
    float hw = width / 2, vx = x1 - x0, vy = y1 - y0, len2 = vx * vx + vy * vy;
    for (int y = int(min(y0, y1) - hw - 1); y <= int(max(y0, y1) + hw + 1); y++)
      for (int x = int(min(x0, x1) - hw - 1); x <= int(max(x0, x1) + hw + 1); x++) {
        float px = x + 0.5f - x0, py = y + 0.5f - y0;
        float t = len2 > 0 ? constrain((px * vx + py * vy) / len2, 0.f, 1.f) : 0;
        float dx = px - t * vx, dy = py - t * vy;
        uint8_t a = coverage(hw - sqrtf(dx * dx + dy * dy) + 0.5f);
        if (a) blend(x, y, color, a);
      }
  }

  // ---- Text (ASCII Lato bitmaps, 4-bit coverage) ----
  static unsigned char glyphIndex(char ch) { unsigned char c = ch; return (c < 32 || c > 126) ? '?' - 32 : c - 32; }
  int textWidth(const String &s, const BitmapFont &f) const {
    int w = 0; for (unsigned i = 0; i < s.length(); i++) w += f.glyphs[glyphIndex(s[i])].advance; return w;
  }
  void text(int x, int y, const String &s, const BitmapFont &f, uint16_t color) {
    if (y >= cy1 || y + f.height <= cy0) return;
    for (unsigned i = 0; i < s.length(); i++) {
      const FontGlyph &g = f.glyphs[glyphIndex(s[i])];
      if (x < cx1 && x + g.width > cx0)
        for (int yy = 0; yy < f.height; yy++) for (int xx = 0; xx < g.width; xx++) {
          unsigned bit = yy * g.width + xx;
          uint8_t packed = f.data[g.offset + bit / 2];
          uint8_t a = bit % 2 ? packed & 15 : packed >> 4;
          if (a) blend(x + xx, y + yy, color, a * 17);
        }
      x += g.advance;
    }
  }
  void textCenter(int cx, int y, const String &s, const BitmapFont &f, uint16_t c) { text(cx - textWidth(s, f) / 2, y, s, f, c); }
  void textRight(int rx, int y, const String &s, const BitmapFont &f, uint16_t c) { text(rx - textWidth(s, f), y, s, f, c); }

  String fit(String value, const BitmapFont &f, int width) const {
    if (textWidth(value, f) <= width) return value;
    while (value.length() > 1 && textWidth(value + "...", f) > width) value.remove(value.length() - 1);
    value.trim();
    return value + "...";
  }
  // Word wrap into at most maxLines lines; a cut-off last line ends in "...".
  std::vector<String> wrap(const String &s, const BitmapFont &f, int width, int maxLines = 1000) const {
    std::vector<String> lines;
    bool truncated = false;
    int start = 0, n = s.length();
    while (true) {
      int nl = s.indexOf('\n', start); if (nl < 0) nl = n;
      String para = s.substring(start, nl), line;
      int i = 0, pn = para.length();
      if (!pn) { if (int(lines.size()) >= maxLines) { truncated = true; break; } lines.push_back(""); }
      while (i < pn) {
        int sp = para.indexOf(' ', i); if (sp < 0) sp = pn;
        String word = para.substring(i, sp);
        String candidate = line.length() ? line + " " + word : word;
        if (textWidth(candidate, f) <= width) { line = candidate; i = sp + 1; continue; }
        if (int(lines.size()) >= maxLines) { truncated = true; break; }
        if (line.length()) { lines.push_back(line); line = ""; continue; }
        int cut = word.length();  // a single word wider than the line: hard-break it
        while (cut > 1 && textWidth(word.substring(0, cut), f) > width) cut--;
        lines.push_back(word.substring(0, cut)); i += cut;
      }
      if (truncated) break;
      if (line.length()) { if (int(lines.size()) >= maxLines) { truncated = true; break; } lines.push_back(line); }
      if (nl >= n) break;
      start = nl + 1;
    }
    if (truncated && !lines.empty()) {
      String l = lines.back();
      while (l.length() && textWidth(l + "...", f) > width) l.remove(l.length() - 1);
      l.trim(); lines.back() = l + "...";
    }
    return lines;
  }

  // ---- Components ----
  // Rounded pill with optional status dot; returns its width.
  int pill(int x, int y, const String &label, const BitmapFont &f, uint16_t fg, uint16_t bg, int h, uint16_t dot = 0) {
    int pad = h / 2, dotW = dot ? h / 3 + 8 : 0;
    int w = textWidth(label, f) + pad * 2 + dotW;
    fillRoundRect(x, y, w, h, h / 2, bg);
    if (dot) fillCircle(x + pad + h / 6.f, y + h / 2.f, h / 6.f, dot);
    text(x + pad + dotW, y + (h - f.height) / 2, label, f, fg);
    return w;
  }
  int pillWidth(const String &label, const BitmapFont &f, int h, bool dot) const {
    return textWidth(label, f) + h + (dot ? h / 3 + 8 : 0);
  }
  void progress(int x, int y, int w, int h, float frac, uint16_t fg, uint16_t track) {
    fillRoundRect(x, y, w, h, h / 2, track);
    int fw = int(w * constrain(frac, 0.f, 1.f));
    if (fw > 0) fillRoundRect(x, y, max(fw, h), h, h / 2, fg);
  }
  void spinner(int cx, int cy, int r, uint16_t color, uint16_t track) {
    arc(cx, cy, r, 4, 0, 360, track);
    arc(cx, cy, r, 4, float((millis() / 3) % 360), 100, color);
  }
  void check(int cx, int cy, int r, uint16_t color) {
    fillCircle(cx, cy, r, color);
    stroke(cx - r * 0.45f, cy + r * 0.02f, cx - r * 0.1f, cy + r * 0.38f, 3, Theme::BG);
    stroke(cx - r * 0.1f, cy + r * 0.38f, cx + r * 0.48f, cy - r * 0.34f, 3, Theme::BG);
  }
  void wifiIcon(int cx, int bottom, int size, uint16_t color) {
    for (int i = 1; i <= 3; i++) arc(cx, bottom, size * i / 3.f, 2.6f, 225, 90, color);
    fillCircle(cx, bottom - 1, 2.4f, color);
  }
  void usbIcon(int cx, int cy, int size, uint16_t color) {
    float s = size / 2.f;
    stroke(cx - s, cy, cx + s, cy, 2.4f, color);
    fillCircle(cx - s, cy, 3, color);
    stroke(cx - s * 0.2f, cy, cx + s * 0.25f, cy - s * 0.55f, 2.2f, color);
    stroke(cx + s * 0.25f, cy - s * 0.55f, cx + s * 0.6f, cy - s * 0.55f, 2.2f, color);
    stroke(cx + s * 0.05f, cy, cx + s * 0.45f, cy + s * 0.5f, 2.2f, color);
    fillRoundRect(cx + s * 0.45f, cy + s * 0.5f - 3, 6, 6, 1, color);
    stroke(cx + s, cy - 4, cx + s, cy + 4, 2.4f, color);
  }
  void chevronLeft(int cx, int cy, int s, uint16_t color) {
    stroke(cx + s / 2.f, cy - s, cx - s / 2.f, cy, 3, color);
    stroke(cx - s / 2.f, cy, cx + s / 2.f, cy + s, 3, color);
  }
  void chevronDown(int cx, int cy, int s, uint16_t color) {
    stroke(cx - s, cy - s / 2.f, cx, cy + s / 2.f, 2.6f, color);
    stroke(cx, cy + s / 2.f, cx + s, cy - s / 2.f, 2.6f, color);
  }
  // Crescent: a disc with an offset disc of the background color on top.
  void moon(int cx, int cy, float r, uint16_t color, uint16_t bg) {
    fillCircle(cx, cy, r, color);
    fillCircle(cx + r * 0.5f, cy - r * 0.35f, r * 0.85f, bg);
  }
  // Brand mark: three overlapping status lights.
  void logo(int cx, int cy, int r) {
    fillCircle(cx - r * 0.9f, cy, r, Theme::WORKING, 230);
    fillCircle(cx, cy, r, Theme::WAITING, 200);
    fillCircle(cx + r * 0.9f, cy, r, Theme::DONE, 200);
  }
};
