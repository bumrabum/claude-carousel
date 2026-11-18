// The carousel window: shows one card at a time, flip with arrows / wheel / swipe.
// Data and actions come from the Rust side (src-tauri/src/main.rs).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Source = "code" | "cowork" | "cli";

interface Card {
  id: string;
  source: Source;
  sessionId: string;
  title: string;
  project: string;
  cwd: string;
  summary: string;
  activityAt: number;
}

interface View {
  cards: Card[];
  autoHideSec: number;
}

const SOURCE_LABEL: Record<Source, string> = { code: "Code", cowork: "Cowork", cli: "CLI" };

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const el = {
  card: $("card"),
  source: $("source"),
  where: $("where"),
  counter: $("counter"),
  title: $("title"),
  summary: $("summary"),
  body: $("body"),
};

let cards: Card[] = [];
let index = 0;
let autoHideSec = 15;
let hideTimer: number | undefined;
let hovered = false;

// ---------- data ----------

async function refresh() {
  const currentId = cards[index]?.id;
  const view = await invoke<View>("get_cards");
  cards = view.cards;
  autoHideSec = view.autoHideSec;
  // Stay on the same card if it is still there.
  const kept = cards.findIndex((c) => c.id === currentId);
  index = kept >= 0 ? kept : Math.min(index, Math.max(cards.length - 1, 0));
  render();
  if (cards.length === 0) hide();
}

// ---------- drawing ----------

function render(direction = 0) {
  const card = cards[index];
  if (!card) return;
  el.card.dataset.source = card.source;
  el.source.textContent = SOURCE_LABEL[card.source];
  el.where.textContent = [card.project, timeAgo(card.activityAt)].filter(Boolean).join(" · ");
  el.counter.textContent = `${index + 1} / ${cards.length}`;
  el.title.textContent = card.title;
  el.summary.textContent = card.summary || "Claude ждёт вашего ответа.";
  el.card.title = card.cwd;
  if (direction !== 0) {
    el.body.classList.remove("slide-left", "slide-right");
    void el.body.offsetWidth; // restart the animation
    el.body.classList.add(direction > 0 ? "slide-left" : "slide-right");
  }
}

function timeAgo(ms: number): string {
  const min = Math.max(1, Math.round((Date.now() - ms) / 60_000));
  if (min < 60) return `${min} мин назад`;
  const h = Math.round(min / 60);
  if (h < 48) return `${h} ч назад`;
  return `${Math.round(h / 24)} дн назад`;
}

function move(step: number) {
  if (cards.length < 2) return;
  index = (index + step + cards.length) % cards.length;
  render(step);
}

// ---------- actions ----------

async function act(command: "open_card" | "mark_done" | "snooze") {
  const card = cards[index];
  if (!card) return;
  await invoke(command, { id: card.id });
  await refresh();
}

function hide() {
  clearTimeout(hideTimer);
  invoke("hide_window");
}

/** Hide by itself unless the mouse is over the card. */
function startHideTimer() {
  clearTimeout(hideTimer);
  if (!hovered) hideTimer = window.setTimeout(hide, autoHideSec * 1000);
}

// ---------- wiring ----------

$("prev").onclick = () => move(-1);
$("next").onclick = () => move(1);
$("close").onclick = hide;
$("open").onclick = () => act("open_card");
$("later").onclick = () => act("snooze");
$("done").onclick = () => act("mark_done");

el.card.addEventListener("mouseenter", () => {
  hovered = true;
  clearTimeout(hideTimer);
});
el.card.addEventListener("mouseleave", () => {
  hovered = false;
  startHideTimer();
});

// Wheel / trackpad swipe flips cards; a short cooldown keeps one swipe = one card.
let wheelLock = 0;
el.card.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    const delta = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
    if (Math.abs(delta) < 8 || Date.now() < wheelLock) return;
    wheelLock = Date.now() + 350;
    move(delta > 0 ? 1 : -1);
  },
  { passive: false },
);

window.addEventListener("keydown", (e) => {
  if (e.key === "ArrowRight" || e.key === "ArrowDown") move(1);
  else if (e.key === "ArrowLeft" || e.key === "ArrowUp") move(-1);
  else if (e.key === "Escape") hide();
  else if (e.key === "Enter") act("open_card");
});

listen("cards-changed", refresh);
listen("shown", async () => {
  index = 0;
  await refresh();
  startHideTimer();
});

refresh();
