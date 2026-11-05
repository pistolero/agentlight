#pragma once
#include <algorithm>
#include <cstdint>
#include <string>
#include <vector>

#ifndef SLEEP_TIMEOUT_MS
#define SLEEP_TIMEOUT_MS (5u * 60u * 1000u)
#endif

// Sleep mode rules, free of hardware so they can be tested on the host.
// The screen stays on while the mode is off. When on, it turns off after the
// timeout, unless an agent has newly started needing attention: that holds the
// screen on until the agent's status changes or the user re-activates the mode.
struct AgentStatus { std::string id, status; };

class PowerMode {
public:
  bool active() const { return active_; }
  bool alerting() const { return !alerting_.empty(); }
  bool screenOn() const { return screenOn_; }

  // Restore the saved mode after a reboot; the screen starts on.
  void restore(bool active, uint32_t now) { active_ = active; screenOn_ = true; lastActivity_ = now; }

  // The Sleep control: activate; or during an alert re-arm; otherwise leave.
  void toggle(uint32_t now) {
    screenOn_ = true;
    lastActivity_ = now;
    if (active_ && alerting_.empty()) active_ = false;
    else active_ = true;
    alerting_.clear();  // these agents stay in known_, so they count as seen
  }

  // Returns true if this touch only woke the screen and must not press anything.
  bool touch(uint32_t now) {
    lastActivity_ = now;
    if (screenOn_) return false;
    screenOn_ = true;
    return true;
  }

  // Feed each state frame. Returns true when this frame started an alert.
  bool statuses(const std::vector<AgentStatus> &agents, uint32_t now) {
    std::vector<std::string> attention;
    for (const AgentStatus &a : agents)
      if (a.status == "waiting" || a.status == "error") attention.push_back(a.id);

    std::vector<std::string> fresh;
    for (const std::string &id : attention)
      if (std::find(known_.begin(), known_.end(), id) == known_.end()) fresh.push_back(id);
    bool baseline = !seenFrame_;  // the first frame after boot shows nothing new
    seenFrame_ = true;
    known_ = attention;

    // Alerting agents whose status changed, or which closed, no longer hold the screen.
    bool held = !alerting_.empty();
    alerting_.erase(std::remove_if(alerting_.begin(), alerting_.end(), [&](const std::string &id) {
      return std::find(attention.begin(), attention.end(), id) == attention.end();
    }), alerting_.end());
    if (held && alerting_.empty()) lastActivity_ = now;

    if (!active_ || baseline || fresh.empty()) return false;
    bool started = alerting_.empty();
    alerting_.insert(alerting_.end(), fresh.begin(), fresh.end());
    screenOn_ = true;
    return started;
  }

  void tick(uint32_t now) {
    if (active_ && alerting_.empty() && screenOn_ && now - lastActivity_ >= SLEEP_TIMEOUT_MS) screenOn_ = false;
  }

private:
  bool active_ = false, screenOn_ = true, seenFrame_ = false;
  uint32_t lastActivity_ = 0;
  std::vector<std::string> known_, alerting_;
};
