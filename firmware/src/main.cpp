#include <Arduino.h>
#include <ArduinoJson.h>
#include <esp_display_panel.hpp>
#include <esp_lcd_panel_rgb.h>
#include <vector>
#include "wifi_link.h"
#include "app_state.h"
#include "ui_boot.h"
#include "ui_grid.h"
#include "ui_log.h"
#include "ui_ota.h"

using namespace esp_panel::board;
using namespace esp_panel::drivers;

Board board;
uint16_t *panelBuffers[2] = {nullptr, nullptr};
int drawBuffer = 1, swapWait = 0;
DRAM_ATTR SemaphoreHandle_t frameReady = nullptr;
IRAM_ATTR bool frameFinished(void *data) {
  BaseType_t wake = pdFALSE;
  auto semaphore = *static_cast<SemaphoreHandle_t *>(data);
  xSemaphoreGiveFromISR(semaphore, &wake);
  return wake == pdTRUE;
}
bool finishBufferSwap() {
  // Two boundaries also cover RGB's PSRAM-to-bounce-buffer pipeline. Never
  // draw into the previous scanout buffer until the switch is safely complete.
  while (swapWait) {
    if (xSemaphoreTake(frameReady, pdMS_TO_TICKS(500)) != pdTRUE) return false;
    --swapWait;
  }
  drawBuffer = 1 - drawBuffer; canvas.pixels = panelBuffers[drawBuffer]; return true;
}

// Pixel clock: refresh = PCLK / (1368 x 637 clocks per frame). The panel and
// network share PSRAM bandwidth: 14 MHz (~16 Hz) is safe for the Wi-Fi TLS
// handshake and OTA downloads but visibly shimmers; 21 MHz (~24 Hz) looks
// steady. 18 MHz builds failed to log in over Wi-Fi at least intermittently
// (0.4.2 twice; 0.5.2 once, then it connected), reason unknown. Run fast, and
// slow down only while heavy network work is in flight.
constexpr uint32_t PCLK_SAFE = 14000000, PCLK_FAST = 21000000;
constexpr uint32_t PCLK_SLEEP = 8000000;  // nobody can see the scan with the backlight off
uint32_t pclk = PCLK_SAFE, lastHeavy = 0;
void updatePixelClock() {
  bool heavy = Ota::phase != Ota::Idle || WifiLink::linkState == WifiLink::Connecting;
  if (heavy) lastHeavy = millis();
  uint32_t want = !power.screenOn() ? PCLK_SLEEP : heavy || millis() - lastHeavy < 2000 ? PCLK_SAFE : PCLK_FAST;
  if (want == pclk) return;
  // Applied by the driver at the next VSYNC, so the picture is not disturbed.
  if (esp_lcd_rgb_panel_set_pclk(board.getLCD()->getRefreshPanelHandle(), want) == ESP_OK) pclk = want;
}
bool dirty = true;
uint32_t lastDraw = 0, lastTouch = 0, lastUsbState = 0;
char line[4097]; size_t lineLen = 0; bool overflow = false;

// ---------------------------------------------------------------- protocol
void hello() {
  JsonDocument doc; doc["v"] = 1; doc["t"] = "hello"; doc["device"] = "managents"; doc["fw"] = FW_VERSION; doc["build"] = Ota::build();
  doc["board"] = "waveshare-5b"; doc["w"] = WIDTH; doc["h"] = HEIGHT; doc["proto"] = 1; doc["logv"] = 2;
  doc["wifi_setup"] = true; doc["device_id"] = WifiLink::deviceId(); serializeJson(doc, Serial); Serial.println();
}
void sendToHelper(const JsonDocument &doc) {
  if (usingWifi) { String message; serializeJson(doc, message); WifiLink::send(message); }
  else { serializeJson(doc, Serial); Serial.println(); }
}
void requestLog() {
  JsonDocument request; request["v"] = 1; request["t"] = "tail"; request["id"] = logView.agentId;
  if (logView.childId.length()) request["child"] = logView.childId;
  sendToHelper(request);
  logView.requestedAt = millis();
}
void openLog(int agentIndex, int childIndex) {
  const Agent &a = agents[agentIndex];
  logView = LogView();
  logView.open = true; logView.agentId = a.id;
  String agentTitle = a.activity.length() ? a.activity : a.name;
  if (childIndex < 0) {
    logView.title = agentTitle;
    logView.contextA = a.activity.length() ? a.name : String();
    logView.contextB = kindLabel(a.kind);
    logView.agentLabel = kindLabel(a.kind);
  } else {
    const Child &c = a.children[childIndex];
    logView.childId = c.id; logView.title = c.name;
    logView.contextA = "Subagent"; logView.contextB = agentTitle;
    logView.agentLabel = "Subagent";
  }
  requestLog();
}
void closeLog() { logView = LogView(); }

void acceptLog(JsonObjectConst doc) {
  if (!logView.open || doc["id"] != logView.agentId || String(doc["child"] | "") != logView.childId) return;
  std::vector<LogEntry> next;
  String state = "ok";
  if (doc["entries"].is<JsonArrayConst>()) {
    for (JsonVariantConst item : doc["entries"].as<JsonArrayConst>()) {
      const char *role = item[0] | "a";
      if (item[1].is<const char *>()) next.push_back({role[0], item[1].as<String>()});
    }
    state = doc["state"] | "ok";
  } else if (doc["text"].is<const char *>()) {
    // Older helpers send "role: text" lines; continuation lines belong to the previous entry.
    String text = doc["text"].as<String>();
    if (text.startsWith("Log unavailable")) state = "unavailable";
    else if (text.startsWith("No recent readable")) state = "empty";
    else for (int start = 0; start <= int(text.length());) {
      int end = text.indexOf('\n', start); if (end < 0) end = text.length();
      String row = text.substring(start, end);
      if (row.startsWith("user: ")) next.push_back({'u', row.substring(6)});
      else if (row.startsWith("assistant: ")) next.push_back({'a', row.substring(11)});
      else if (row.startsWith("Tool: ")) next.push_back({'t', row.substring(6)});
      else if (next.size()) next.back().text += "\n" + row;
      else next.push_back({'a', row});
      start = end + 1;
    }
  } else return;
  bool changed = next.size() != logView.entries.size();
  for (size_t i = 0; !changed && i < next.size(); i++)
    changed = next[i].role != logView.entries[i].role || next[i].text != logView.entries[i].text;
  if (changed) { logView.entries = std::move(next); logView.layoutDirty = true; logView.revision++; }
  if (state.isEmpty()) state = "ok";
  if (state != logView.state) { logView.state = state; changed = true; }
  if (changed) dirty = true;
}

bool parseState(JsonObjectConst root) {
  if (!root["now"].is<uint64_t>() || !root["agents"].is<JsonArrayConst>()) return false;
  JsonArrayConst list = root["agents"].as<JsonArrayConst>();
  if (list.size() > MAX_AGENTS) return false;
  static Agent next[MAX_AGENTS]; int n = 0;
  for (JsonObjectConst a : list) {
    if (!a["id"].is<const char *>() || !a["kind"].is<const char *>() || !a["name"].is<const char *>() || !a["status"].is<const char *>() || !a["age"].is<uint32_t>()) return false;
    Agent &v = next[n++]; v.id = a["id"].as<String>(); v.kind = a["kind"].as<String>(); v.name = a["name"].as<String>(); v.activity = a["activity"] | ""; v.status = a["status"].as<String>(); v.age = a["age"].as<uint32_t>();
    if (v.id.isEmpty() || v.id.length() > 48 || v.name.length() > 64 || (v.status != "working" && v.status != "waiting" && v.status != "error" && v.status != "idle")) return false;
    v.tasksDone = 0; v.tasksTotal = 0;
    if (a["tasks"].is<JsonObjectConst>()) {
      JsonObjectConst tasks = a["tasks"].as<JsonObjectConst>();
      if (!tasks["completed"].is<uint32_t>() || !tasks["total"].is<uint32_t>()) return false;
      v.tasksDone = tasks["completed"].as<uint32_t>(); v.tasksTotal = tasks["total"].as<uint32_t>();
      if (!v.tasksTotal || v.tasksDone > v.tasksTotal) return false;
    } else if (!a["tasks"].isNull()) return false;
    v.childCount = 0;
    if (a["children"].is<JsonArrayConst>()) {
      JsonArrayConst children = a["children"].as<JsonArrayConst>();
      if (children.size() > MAX_CHILDREN) return false;
      for (JsonVariantConst item : children) {
        if (!item.is<JsonObjectConst>()) return false;
        JsonObjectConst child = item.as<JsonObjectConst>();
        if (!child["id"].is<const char *>() || !child["name"].is<const char *>() || !child["status"].is<const char *>() || !child["age"].is<uint32_t>()) return false;
        Child &c = v.children[v.childCount++]; c.id = child["id"].as<String>(); c.name = child["name"].as<String>(); c.status = child["status"].as<String>(); c.age = child["age"].as<uint32_t>(); c.activity = child["activity"] | "";
        if (c.id.isEmpty() || c.id.length() > 48 || c.name.length() > 40 || c.activity.length() > 96 || (c.status != "working" && c.status != "waiting" && c.status != "error" && c.status != "done")) return false;
      }
    } else if (!a["children"].isNull()) return false;
    v.used = 0; v.limit = 0;
    if (a["ctx"].is<JsonObjectConst>()) {
      auto ctx = a["ctx"].as<JsonObjectConst>();
      if (!ctx["used"].is<uint32_t>()) return false;
      v.used = ctx["used"].as<uint32_t>();
      if (!ctx["limit"].isNull()) { if (!ctx["limit"].is<uint32_t>() || ctx["limit"].as<uint32_t>() == 0) return false; v.limit = ctx["limit"].as<uint32_t>(); }
    }
  }
  for (int i = 0; i < n; i++) agents[i] = next[i];
  agentCount = n;
  moreAgents = root["more"] | 0; hostTime = root["now"].as<uint64_t>(); tzOffset = root["tz"] | 0;
  receivedAt = millis(); everConnected = true;  // redraws follow from screenKey() changes
  std::vector<AgentStatus> statuses;
  for (int i = 0; i < n; i++) statuses.push_back({agents[i].id.c_str(), agents[i].status.c_str()});
  if (power.statuses(statuses, receivedAt)) closeLog();
  return true;
}
void acceptMessage(const char *message, bool usb) {
  JsonDocument doc;
  if (deserializeJson(doc, message) || doc["v"] != 1) return;
  if (usb && doc["t"] == "wifi_config") {
    bool ok = WifiLink::configure(doc.as<JsonObjectConst>());
    Serial.printf("{\"v\":1,\"t\":\"wifi_configured\",\"ok\":%s}\n", ok ? "true" : "false");
    return;
  }
  if (usb && doc["t"] == "hello") hello();
  else if (doc["t"] == "tail") acceptLog(doc.as<JsonObjectConst>());
  else if (doc["t"] == "state") {
    if (!usb && lastUsbState && millis() - lastUsbState < STALE_MS) return;
    if (parseState(doc.as<JsonObjectConst>())) {
      usingWifi = !usb;
      if (usb) { lastUsbState = millis(); Ota::confirmIfPending(); Serial.println("{\"v\":1,\"t\":\"ack\"}"); }
    }
  }
}

// ---------------------------------------------------------------- input
struct TouchState { bool down = false, dragging = false; int x0 = 0, y0 = 0, x = 0, y = 0, scroll0 = 0, empty = 0; bool ignore = false; } touch;

void saveSleepMode() {
  Preferences prefs;
  if (prefs.begin("agentlight", false)) { prefs.putBool("sleep", power.active()); prefs.end(); }
}

void act(const Hit &h) {
  int pages = pageCount();
  switch (h.action) {
    case OpenAgent: if (h.agent < agentCount) openLog(h.agent, -1); break;
    case OpenChild: if (h.agent < agentCount && h.child < agents[h.agent].childCount) openLog(h.agent, h.child); break;
    case NextPage: page = (page + 1) % pages; break;
    case PrevPage: page = (page + pages - 1) % pages; break;
    case Back: closeLog(); break;
    case Follow: logView.follow = true; logView.scroll = logMaxScroll(); requestLog(); break;
    case ToggleSleep: {
      bool was = power.active();
      power.toggle(millis());
      if (power.active() != was) saveSleepMode();
      break;
    }
  }
  dirty = true;
}
void tap(int x, int y) {
  for (int i = hitCount - 1; i >= 0; i--) {
    const Hit &h = hits[i];
    if (x >= h.x && x < h.x + h.w && y >= h.y && y < h.y + h.h) { act(h); return; }
  }
  // Empty grid space pages forward, as before.
  if (!logView.open && fresh() && pageCount() > 1 && y > GridScreen::BAR) { page = (page + 1) % pageCount(); dirty = true; }
}
void handleTouch() {
  auto panel = board.getTouch();
  if (!panel) return;
  panel->readRawData(-1, -1, 0);
  std::vector<TouchPoint> points; panel->getPoints(points);
  if (!points.empty()) {
    int x = points[0].x, y = points[0].y;
    touch.empty = 0;
    if (!touch.down) {
      // A touch that wakes the screen does nothing else, through to its release.
      bool wokeOnly = power.touch(millis());
      touch = {true, false, x, y, x, y, logView.scroll, 0, wokeOnly};
      return;
    }
    touch.x = x; touch.y = y;
    if (touch.ignore || Ota::phase != Ota::Idle) return;
    if (logView.open && !touch.dragging && touch.y0 > LOG_TOP && abs(y - touch.y0) > 12) { touch.dragging = true; logView.follow = false; }
    if (touch.dragging) {
      int scroll = constrain(touch.scroll0 - (y - touch.y0), 0, logMaxScroll());
      if (scroll != logView.scroll) { logView.scroll = scroll; dirty = true; }
    }
    return;
  }
  // Require two empty polls so a dropped sample isn't taken as a release.
  if (!touch.down || ++touch.empty < 2) return;
  touch.down = false; lastTouch = millis();
  power.touch(lastTouch);  // the countdown runs from the end of the touch
  if (touch.ignore || Ota::phase != Ota::Idle) return;
  if (touch.dragging) {
    touch.dragging = false;
    if (logView.scroll >= logMaxScroll() - 4) { logView.follow = true; requestLog(); }
    dirty = true;
    return;
  }
  int dx = touch.x - touch.x0, dy = touch.y - touch.y0;
  if (!logView.open && fresh() && pageCount() > 1 && abs(dx) > 80 && abs(dx) > 2 * abs(dy)) {
    page = (page + (dx < 0 ? 1 : pageCount() - 1)) % pageCount();
    dirty = true;
    return;
  }
  tap(touch.x0, touch.y0);
}

// ---------------------------------------------------------------- rendering
void render() {
  if (swapWait && !finishBufferSwap()) return;
  hitCount = 0;
  if (Ota::phase != Ota::Idle) OtaScreen::draw();
  else if (logView.open) LogScreen::draw();
  else if (fresh()) GridScreen::draw();
  else BootScreen::draw();
  while (xSemaphoreTake(frameReady, 0) == pdTRUE) {}
  if (board.getLCD()->switchFrameBufferTo(canvas.pixels)) { swapWait = 2; finishBufferSwap(); }
}
// Every full redraw competes with the panel's DMA for PSRAM bandwidth and can
// tear the picture, so screens redraw only when what they show has changed.
uint32_t screenKey() {
  if (Ota::phase != Ota::Idle) return OtaScreen::key();
  if (logView.open) return LogScreen::key();
  if (fresh()) return GridScreen::key();
  return BootScreen::key();
}
uint32_t lastKey = 0;

bool backlightOn = true;
void applyScreen() {
  bool on = power.screenOn();
  if (on == backlightOn) return;
  auto backlight = board.getBacklight();
  backlightOn = on;
  if (!on) { if (backlight) backlight->off(); return; }
  // Paint a fresh frame first, so the backlight doesn't reveal a stale picture.
  updatePixelClock();
  render(); lastKey = screenKey(); dirty = false; lastDraw = millis();
  if (backlight) backlight->on();
}

void setup() {
  Ota::begin();
  { Preferences prefs; bool sleep = false;
    if (prefs.begin("agentlight", true)) { sleep = prefs.getBool("sleep", false); prefs.end(); }
    power.restore(sleep, millis()); }
  Serial.begin(115200);
  // A complete protocol frame can be 4096 bytes; the HWCDC default is only 256.
  Serial.setRxBufferSize(8192);
  if (!board.init()) { while (true) delay(1000); }
  // Start at the safe pixel clock; updatePixelClock() raises it once idle.
  // Larger internal bounce buffers keep the scan steady under network load.
  auto rgb = static_cast<BusRGB *>(board.getLCD()->getBus());
  if (!rgb->configRGB_FreqHz(PCLK_SAFE) || !rgb->configRGB_BounceBufferSize(WIDTH * 20) || !rgb->configRGB_FrameBufferNumber(2) || !board.begin()) {
    Serial.println("RGB initialization failed"); while (true) delay(1000);
  }
  panelBuffers[0] = static_cast<uint16_t *>(board.getLCD()->getFrameBufferByIndex(0));
  panelBuffers[1] = static_cast<uint16_t *>(board.getLCD()->getFrameBufferByIndex(1));
  frameReady = xSemaphoreCreateBinary();
  if (!panelBuffers[0] || !panelBuffers[1] || !frameReady || !board.getLCD()->attachRefreshFinishCallback(frameFinished, &frameReady)) {
    Serial.println("RGB double buffering failed"); while (true) delay(1000);
  }
  canvas.pixels = panelBuffers[drawBuffer];
  if (board.getBacklight()) board.getBacklight()->on();
  WifiLink::begin();
  hello(); render();
}
void loop() {
  while (Serial.available()) {
    char c = Serial.read();
    if (c == '\n') {
      if (!overflow) { if (lineLen && line[lineLen - 1] == '\r') --lineLen; line[lineLen] = 0; acceptMessage(line, true); }
      lineLen = 0; overflow = false;
    } else if (!overflow) { if (lineLen < 4096) line[lineLen++] = c; else overflow = true; }
  }
  static WifiLink::Frame networkFrame;
  while (WifiLink::receive(networkFrame)) acceptMessage(networkFrame.text, false);
  handleTouch();
  if (Ota::phase != Ota::Idle) power.touch(millis());  // an update always shows its progress
  power.tick(millis());
  applyScreen();
  updatePixelClock();
  Ota::checkDeadline();
  if (Ota::phase == Ota::Failed && millis() - Ota::phaseMs > 8000) { Ota::phase = Ota::Idle; dirty = true; }
  if (page && !logView.open && millis() - lastTouch > 30000) { page = 0; dirty = true; }
  if (!power.screenOn()) { delay(50); return; }  // no drawing or log polling in the dark
  if (logView.open && logView.follow && millis() - logView.requestedAt >= 2000) requestLog();
  // Drags redraw at most ~30 fps; otherwise check for visible changes 4x a second.
  uint32_t now = millis();
  bool due = touch.dragging ? now - lastDraw >= 33 : now - lastDraw >= 250;
  if (due && (dirty || screenKey() != lastKey)) { render(); lastKey = screenKey(); dirty = false; lastDraw = now; }
  delay(touch.dragging ? 5 : 15);
}
