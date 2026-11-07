#pragma once
#include <Arduino.h>
#include <vector>
#include "ui_canvas.h"
#include "power_mode.h"

// Data model shared by the protocol parser, input handling and screens.
constexpr int MAX_AGENTS = 24, PAGE_SIZE = 9, MAX_CHILDREN = 6, MAX_LIMITS = 2;
constexpr uint32_t STALE_MS = 6000;

struct Child { String id, name, status, activity; uint32_t age; };
struct PlanLimit { String label; uint8_t percent; };
struct Agent {
  String id, kind, name, activity, status;
  uint32_t age, used, limit, tasksDone, tasksTotal;
  Child children[MAX_CHILDREN];
  int childCount;
  PlanLimit limits[MAX_LIMITS];
  int limitCount;
};

inline Canvas canvas;
inline PowerMode power;
inline Agent agents[MAX_AGENTS];
inline int agentCount = 0, moreAgents = 0, page = 0;
inline uint32_t receivedAt = 0;
inline uint64_t hostTime = 0;
inline int32_t tzOffset = 0;
inline bool everConnected = false, usingWifi = false;

inline bool fresh() { return everConnected && millis() - receivedAt < STALE_MS; }
inline uint32_t elapsed(uint32_t age) { return age + (millis() - receivedAt) / 1000; }
inline int pageCount() { return max(1, (agentCount + PAGE_SIZE - 1) / PAGE_SIZE); }
inline const Agent *findAgent(const String &id) {
  for (int i = 0; i < agentCount; i++) if (agents[i].id == id) return &agents[i];
  return nullptr;
}
inline const Child *findChild(const Agent *a, const String &id) {
  if (a) for (int j = 0; j < a->childCount; j++) if (a->children[j].id == id) return &a->children[j];
  return nullptr;
}

// Tap targets, rebuilt by every render. Later entries sit on top.
enum Action : uint8_t { OpenAgent, OpenChild, NextPage, PrevPage, Back, Follow, ToggleSleep };
struct Hit { int16_t x, y, w, h; uint8_t action; int8_t agent, child; };
inline Hit hits[96];
inline int hitCount = 0;
inline void addHit(int x, int y, int w, int h, Action action, int agent = -1, int child = -1) {
  if (hitCount < int(sizeof(hits) / sizeof(hits[0]))) hits[hitCount++] = {int16_t(x), int16_t(y), int16_t(w), int16_t(h), action, int8_t(agent), int8_t(child)};
}

// Live log of one agent or subagent.
struct LogEntry { char role; String text; };
enum LineKind : uint8_t { LabelLine, BodyLine, ToolLine };
struct LogLine { LineKind kind; char role; int y; String text; };
struct LogView {
  bool open = false;
  String agentId, childId, title, contextA, contextB, agentLabel;
  String state = "loading";  // loading | ok | empty | unavailable
  std::vector<LogEntry> entries;
  std::vector<LogLine> lines;
  bool layoutDirty = true, follow = true;
  int contentHeight = 0, scroll = 0;
  uint32_t requestedAt = 0, revision = 0;
};
inline LogView logView;
constexpr int LOG_TOP = 76, LOG_PAD = 20;
inline int logViewHeight() { return HEIGHT - LOG_TOP; }
inline int logMaxScroll() { return max(0, logView.contentHeight + 2 * LOG_PAD - logViewHeight()); }

// FNV-1a over everything a screen shows; equal keys mean an identical frame.
struct FrameKey {
  uint32_t h = 2166136261u;
  void add(const String &s) { for (unsigned i = 0; i < s.length(); i++) { h ^= uint8_t(s[i]); h *= 16777619u; } add(uint32_t(s.length())); }
  void add(uint32_t v) { for (int i = 0; i < 4; i++) { h ^= (v >> (8 * i)) & 0xff; h *= 16777619u; } }
};

// "first  .  second" with a drawn dot separator; returns the total width.
inline int dotted(int x, int y, const String &a, const String &b, const BitmapFont &f, uint16_t color, bool draw = true) {
  int w = canvas.textWidth(a, f);
  if (!b.length()) { if (draw) canvas.text(x, y, a, f, color); return w; }
  if (!a.length()) { if (draw) canvas.text(x, y, b, f, color); return canvas.textWidth(b, f); }
  if (draw) {
    canvas.text(x, y, a, f, color);
    canvas.fillCircle(x + w + 9, y + f.height * 0.55f, 2, color);
    canvas.text(x + w + 18, y, b, f, color);
  }
  return w + 18 + canvas.textWidth(b, f);
}
