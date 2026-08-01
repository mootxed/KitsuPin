import "./styles.css";
import "./category-editor.css";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, isTauri } from "./api";
import type { Category, ClipSummary, ClipQuery, ContentType, Grouping, Settings } from "./types";
import { hydrateIcons } from "./ui/icons";
import { mainWindowShell } from "./ui/main-window";
import { popupShell } from "./ui/popup";
import { relativeTime } from "./ui/time";

const popup = new URLSearchParams(location.search).get("mode") === "popup";
const state: {
  clips: ClipSummary[];
  categories: Category[];
  settings: Settings | null;
  search: string;
  type: ContentType | null;
  category: string | null;
  domain: string | null;
  grouping: Grouping;
  selected: number;
  hasMore: boolean;
} = {
  clips: [],
  categories: [],
  settings: null,
  search: "",
  type: null,
  category: null,
  domain: null,
  grouping: "none",
  selected: 0,
  hasMore: false,
};

/** Tracks whether the invalid-settings warning has already been shown this session. */
let invalidWarningConsumed = false;

const app = document.querySelector<HTMLElement>("#app")!;
const esc = (value: string) =>
  value.replace(/[&<>'"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" }[c]!));
const tag = (text: string, className = "", style = "") => `<span class="tag ${className}" ${style}>${esc(text)}</span>`;

// ── Toast notifications ───────────────────────────────────────────────────────

let toastTimer = 0;
function showToast(message: string, type: "error" | "info" = "error") {
  let container = document.querySelector<HTMLElement>("#toast-container");
  if (!container) {
    container = document.createElement("div");
    container.id = "toast-container";
    container.setAttribute("aria-live", "assertive");
    container.style.cssText =
      "position:fixed;bottom:24px;right:24px;z-index:9999;display:flex;flex-direction:column;gap:8px;max-width:360px;";
    document.body.appendChild(container);
  }
  const toast = document.createElement("div");
  toast.className = `toast toast-${type}`;
  toast.textContent = message;
  toast.style.cssText =
    `padding:10px 16px;border-radius:8px;font-size:13px;line-height:1.4;pointer-events:auto;` +
    (type === "error"
      ? "background:#b91c1c;color:#fff;"
      : "background:#1e293b;color:#e2e8f0;");
  container.appendChild(toast);
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    toast.remove();
  }, 4000);
}

// ── Utilities ─────────────────────────────────────────────────────────────────

/** Returns true if the event target is an interactive element that should receive
 *  keyboard events exclusively (button, input, select, textarea, [role=button]).
 *  Use to prevent card-level Enter handler from firing when focus is on a control. */
function isInteractiveTarget(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false;
  return !!el.closest('button, input, select, textarea, [role="button"], a');
}

function query(): ClipQuery {
  return {
    search: state.search || undefined,
    contentType: state.type || undefined,
    categoryId: state.category || undefined,
    domain: state.domain || undefined,
    limit: popup ? 16 : 60,
  };
}

async function refresh() {
  state.clips = await api.list(query());
  state.hasMore = !popup && state.clips.length === 60;
  state.selected = Math.min(state.selected, Math.max(0, state.clips.length - 1));
  renderFilters();
  renderCards();
}

// ── Filters ───────────────────────────────────────────────────────────────────

function filtersMarkup(): string {
  const types: ContentType[] = ["Text", "Links", "Email", "Numbers"];
  const domains = [...new Set(state.clips.map((c) => c.domain).filter((v): v is string => !!v))].slice(0, 5);
  return `<div class="filter-rail" role="toolbar" aria-label="Фильтры"><button class="filter ${
    !state.type && !state.category && !state.domain ? "active" : ""
  }" data-filter="all">Все</button>${types
    .map((t) => `<button class="filter type-${t.toLowerCase()} ${state.type === t ? "active" : ""}" data-type="${t}">${t}</button>`)
    .join("")}${state.categories
    .map(
      (c) =>
        `<button class="filter user-filter ${state.category === c.id ? "active" : ""}" data-category="${c.id}" style="--tag:${
          c.color
        }">${esc(c.name)}</button>`
    )
    .join("")}${domains
    .map((d) => `<button class="filter domain-filter ${state.domain === d ? "active" : ""}" data-domain="${esc(d)}">${esc(d)}</button>`)
    .join("")}${popup ? "" : `<button class="filter add-category" data-action="new-category"><i data-lucide="plus"></i> Категория</button>`}</div>`;
}

function renderFilters() {
  const root = document.querySelector<HTMLElement>("#filters-container");
  if (!root) return;
  root.innerHTML = filtersMarkup();
  bindFilters(root);
  hydrateIcons(root);
}

function bindFilters(root: HTMLElement) {
  root.querySelectorAll<HTMLElement>("[data-type]").forEach(
    (el) =>
      (el.onclick = () => {
        state.type = state.type === el.dataset.type ? null : (el.dataset.type as ContentType);
        state.category = state.domain = null;
        refresh();
      })
  );
  root.querySelectorAll<HTMLElement>("[data-category]").forEach((el) => {
    el.onclick = () => {
      state.category = state.category === el.dataset.category ? null : el.dataset.category!;
      state.type = state.domain = null;
      refresh();
    };
    el.ondragover = (e) => e.preventDefault();
    el.ondrop = (e) => {
      e.preventDefault();
      const id = e.dataTransfer?.getData("text/kitsupin");
      if (id) api.assign(id, el.dataset.category!).then(reload).catch(() => showToast("Не удалось назначить категорию"));
    };
  });
  root.querySelectorAll<HTMLElement>("[data-domain]").forEach(
    (el) =>
      (el.onclick = () => {
        state.domain = state.domain === el.dataset.domain ? null : el.dataset.domain!;
        state.type = state.category = null;
        refresh();
      })
  );
  const allBtn = root.querySelector<HTMLElement>("[data-filter=all]");
  if (allBtn) {
    allBtn.onclick = () => {
      state.type = state.category = state.domain = null;
      refresh();
    };
  }
  root.querySelector("[data-action=new-category]")?.addEventListener("click", showCategoryModal);
}

// ── Shell ─────────────────────────────────────────────────────────────────────

function renderShell() {
  app.className = popup ? "popup-shell" : "main-shell";
  // No arguments — no duplicate filters-container ID.
  app.innerHTML = popup ? popupShell() : mainWindowShell();
  bindShell();
  hydrateIcons(app);
}

function bindShell() {
  const input = document.querySelector<HTMLInputElement>("#search")!;
  let timer = 0;
  input?.addEventListener("input", () => {
    state.search = input.value;
    clearTimeout(timer);
    timer = window.setTimeout(refresh, 120);
  });
  if (popup && input) setTimeout(() => input.focus(), 20);
  document.querySelector("#grouping")?.addEventListener("change", (e) => {
    state.grouping = (e.target as HTMLSelectElement).value as Grouping;
    renderCards();
  });
  document.querySelector("[data-action=settings]")?.addEventListener("click", showSettings);
}

// ── Card rendering ────────────────────────────────────────────────────────────

function clipCard(c: ClipSummary, index: number) {
  const categories = c.categories
    .map((x) =>
      popup
        ? `<span class="tag user-tag" style="--tag:${x.color}">${esc(x.name)}</span>`
        : `<button class="tag user-tag" style="--tag:${x.color}" data-unassign="${x.id}" title="Убрать категорию">${esc(x.name)} ×</button>`
    )
    .join("");
  // 6.3: use backend is_truncated flag (not JS string.length comparison).
  const isTruncated = c.isTruncated;
  // 6.1: details button only in main window (no #modal-root in popup).
  const detailsButton =
    isTruncated && !popup
      ? `<button data-action="details" aria-label="Просмотреть полностью" title="Просмотреть полностью"><i data-lucide="eye"></i></button>`
      : "";
  return `<article class="clip-card type-edge-${c.contentType.toLowerCase()} ${
    popup && index === state.selected ? "selected" : ""
  }" tabindex="0" data-clip="${c.id}" draggable="${!popup}" aria-label="Скопировать фрагмент"><div class="card-meta">${tag(
    c.contentType,
    `type-${c.contentType.toLowerCase()}`
  )}${c.domain ? tag(c.domain, "domain-tag") : ""}${categories}<time>${relativeTime(
    c.lastCopiedAt
  )}</time></div><p class="preview">${esc(c.preview)}${isTruncated ? "…" : ""}</p>${c.pageTitle ? `<p class="page-title">${esc(c.pageTitle)}</p>` : ""}<div class="card-foot"><span>${
    c.copyCount > 1 ? `скопировано ${c.copyCount} раза` : "одна копия"
  }${isTruncated ? ` · ${c.contentLength} симв.` : ""}</span><div class="card-actions">${detailsButton}${
    popup
      ? ""
      : `<button data-action="pin" aria-label="${c.pinned ? "Открепить" : "Закрепить"}"><i data-lucide="${
          c.pinned ? "pin-off" : "pin"
        }"></i></button><button data-action="delete" aria-label="Удалить"><i data-lucide="trash-2"></i></button>`
  }</div>${c.pinned ? `<i class="pin-corner" data-lucide="pin"></i>` : ""}</div></article>`;
}

function groups(): [string, ClipSummary[]][] {
  if (state.grouping === "none") return [["", state.clips]];
  if (state.grouping === "type")
    return (["Text", "Links", "Email", "Numbers"] as ContentType[]).map((t) => [t, state.clips.filter((c) => c.contentType === t)]);
  if (state.grouping === "domain") {
    const map = new Map<string, ClipSummary[]>();
    const noSource: ClipSummary[] = [];
    for (const c of state.clips) {
      if (c.domain) {
        const list = map.get(c.domain) || [];
        list.push(c);
        map.set(c.domain, list);
      } else {
        noSource.push(c);
      }
    }
    const result: [string, ClipSummary[]][] = Array.from(map.entries());
    if (noSource.length) {
      result.push(["Без источника", noSource]);
    }
    return result;
  }
  return [
    ...state.categories.map((cat) => [cat.name, state.clips.filter((c) => c.categories.some((x) => x.id === cat.id))] as [string, ClipSummary[]]),
    ["Без категории", state.clips.filter((c) => !c.categories.length)],
  ];
}

function renderCards() {
  const root = document.querySelector<HTMLElement>("#cards");
  if (!root) return;
  if (!state.clips.length) {
    root.innerHTML = `<div class="empty"><div class="empty-paper"><i data-lucide="clipboard"></i></div><h3>${
      state.search ? "Ничего не найдено" : "История пока пуста"
    }</h3><p>${state.search ? "Попробуйте другой запрос или сбросьте фильтры." : "Скопируйте текст через Ctrl+C — он появится здесь."}</p></div>`;
    hydrateIcons(root);
    return;
  }
  if (popup) {
    root.innerHTML = state.clips.map(clipCard).join("");
  } else {
    root.innerHTML =
      groups()
        .filter(([, clips]) => clips.length)
        .map(([name, clips]) => `<section class="clip-group">${name ? `<h3>${esc(name)}</h3>` : ""}<div class="clip-grid">${clips.map((c, i) => clipCard(c, i)).join("")}</div></section>`)
        .join("") + `${state.hasMore ? '<button class="load-more" data-load-more>Показать ещё</button>' : ""}`;
  }
  bindCards(root);
  root.querySelector("[data-load-more]")?.addEventListener("click", loadMore);
  hydrateIcons(root);
}

async function loadMore() {
  const next = await api.list({ ...query(), offset: state.clips.length });
  state.clips.push(...next);
  state.hasMore = next.length === 60;
  renderCards();
}

function bindCards(root: HTMLElement) {
  root.querySelectorAll<HTMLElement>("[data-clip]").forEach((card) => {
    const clip = state.clips.find((c) => c.id === card.dataset.clip)!;

    card.onclick = async (e) => {
      // 6.2: ignore clicks on interactive child elements.
      if (isInteractiveTarget(e.target)) return;
      try {
        let content: string | undefined;
        if (!isTauri) {
          content = await api.getClipContent(clip.id);
        }
        await api.copy(clip.id, popup, content);
      } catch {
        showToast("Не удалось скопировать");
      }
    };

    card.onkeydown = async (e) => {
      if (e.key === "Enter") {
        // 6.2: ignore Enter when focus is on an interactive element within the card.
        if (isInteractiveTarget(e.target)) return;
        try {
          let content: string | undefined;
          if (!isTauri) {
            content = await api.getClipContent(clip.id);
          }
          await api.copy(clip.id, popup, content);
        } catch {
          showToast("Не удалось скопировать");
        }
      }
    };

    card.ondragstart = (e) => e.dataTransfer?.setData("text/kitsupin", clip.id);

    card.querySelector<HTMLElement>("[data-action=details]")?.addEventListener("click", () =>
      showClipDetailsModal(clip.id)
    );

    card.querySelector<HTMLElement>("[data-action=pin]")?.addEventListener("click", async () => {
      try {
        await api.pin(clip.id, !clip.pinned);
        await reload();
      } catch {
        showToast(clip.pinned ? "Не удалось открепить" : "Не удалось закрепить");
      }
    });

    // 6.7: confirm before deleting a pinned clip.
    card.querySelector<HTMLElement>("[data-action=delete]")?.addEventListener("click", async () => {
      if (clip.pinned) {
        const confirmed = await confirmModal(
          "Удалить закреплённую карточку?",
          "Карточка закреплена. Удалить её из истории?"
        );
        if (!confirmed) return;
      }
      try {
        await api.remove(clip.id);
        await reload();
      } catch {
        showToast("Не удалось удалить");
      }
    });

    card.querySelectorAll<HTMLElement>("[data-unassign]").forEach(
      (x) =>
        (x.onclick = async () => {
          try {
            await api.unassign(clip.id, x.dataset.unassign!);
            await reload();
          } catch {
            showToast("Не удалось убрать категорию");
          }
        })
    );
  });
}

// ── Modals ────────────────────────────────────────────────────────────────────

function modal(title: string, body: string): HTMLElement | null {
  const root = document.querySelector<HTMLElement>("#modal-root");
  if (!root) return null;
  root.innerHTML = `<div class="modal-backdrop"><section class="modal" role="dialog" aria-modal="true"><header><h2>${title}</h2><button data-close aria-label="Закрыть"><i data-lucide="x"></i></button></header>${body}</section></div>`;
  root.querySelector("[data-close]")?.addEventListener("click", () => (root.innerHTML = ""));
  hydrateIcons(root);
  return root;
}

/** Show a confirmation dialog using our own modal system.
 *  Returns a promise that resolves to true (confirmed) or false (cancelled). */
function confirmModal(title: string, message: string): Promise<boolean> {
  return new Promise((resolve) => {
    const root = document.querySelector<HTMLElement>("#modal-root");
    if (!root) { resolve(false); return; }
    root.innerHTML = `<div class="modal-backdrop"><section class="modal" role="dialog" aria-modal="true"><header><h2>${esc(title)}</h2></header><p style="padding:12px 0">${esc(message)}</p><div class="modal-actions" style="display:flex;gap:8px;justify-content:flex-end"><button data-cancel>Отмена</button><button class="danger" data-confirm>Удалить</button></div></section></div>`;
    hydrateIcons(root);
    root.querySelector("[data-cancel]")?.addEventListener("click", () => { root.innerHTML = ""; resolve(false); });
    root.querySelector("[data-confirm]")?.addEventListener("click", () => { root.innerHTML = ""; resolve(true); });
  });
}

async function showClipDetailsModal(clipId: string) {
  const clip = state.clips.find((c) => c.id === clipId);
  if (!clip) return;
  const content = await api.getClipContent(clipId);
  const root = modal(
    "Детали фрагмента",
    `<div class="clip-details-view"><div class="clip-details-meta"><span>Тип: <b>${clip.contentType}</b></span><span>Длина: <b>${clip.contentLength} символов</b></span>${
      clip.domain ? `<span>Источник: <b>${esc(clip.domain)}</b></span>` : ""
    }</div><pre class="clip-details-content">${esc(content)}</pre><div class="modal-actions" style="margin-top:16px;display:flex;justify-content:flex-end;"><button class="primary" data-action="copy-details">Скопировать в буфер</button></div></div>`
  );
  if (!root) return;
  root.querySelector("[data-action=copy-details]")?.addEventListener("click", async () => {
    try {
      await api.copy(clipId, popup, content);
      root.innerHTML = "";
    } catch {
      showToast("Не удалось скопировать");
    }
  });
}

function showCategoryModal() {
  const existing = state.categories
    .map(
      (c) =>
        `<form class="category-edit" data-edit="${c.id}"><input name="name" maxlength="60" required value="${esc(
          c.name
        )}"/><input name="color" type="color" value="${c.color}"/><button type="submit">Сохранить</button><button type="button" data-delete="${
          c.id
        }"><i data-lucide="trash-2"></i></button></form>`
    )
    .join("");
  const root = modal(
    "Категории",
    `<div class="category-list">${
      existing || "<p class=notice>Пользовательских категорий пока нет.</p>"
    }</div><form id="category-form"><label>Новая категория<input name="name" maxlength="60" required placeholder="Например, Japanese"/></label><label>Цвет<input name="color" type="color" value="#f2a65a"/></label><button class="primary" type="submit">Создать категорию</button></form>`
  );
  if (!root) return;
  root.querySelector<HTMLFormElement>("#category-form")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const fd = new FormData(e.target as HTMLFormElement);
    try {
      await api.createCategory(String(fd.get("name")), String(fd.get("color")));
      root.innerHTML = "";
      await reload();
    } catch {
      showToast("Не удалось создать категорию");
    }
  });
  root.querySelectorAll<HTMLFormElement>("[data-edit]").forEach((form) =>
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const fd = new FormData(form);
      try {
        await api.updateCategory(form.dataset.edit!, String(fd.get("name")), String(fd.get("color")));
        root.innerHTML = "";
        await reload();
      } catch {
        showToast("Не удалось сохранить категорию");
      }
    })
  );
  root.querySelectorAll<HTMLElement>("[data-delete]").forEach(
    (button) =>
      (button.onclick = async () => {
        try {
          await api.deleteCategory(button.dataset.delete!);
          root.innerHTML = "";
          await reload();
        } catch {
          showToast("Не удалось удалить категорию");
        }
      })
  );
}

function showSettings() {
  const s = state.settings;
  if (!s) return;
  const root = modal(
    "Настройки",
    `<form id="settings-form"><label class="switch-row"><span><b>Запись истории</b><small>Отслеживать обычный X11 Clipboard</small></span><input name="recording" type="checkbox" ${
      s.paused ? "" : "checked"
    }/></label><label class="switch-row"><span><b>Автозапуск</b><small>Запускать после входа в KDE</small></span><input name="autostart" type="checkbox" ${
      s.autostart ? "checked" : ""
    }/></label><label>Горячая клавиша<input name="shortcut" value="${esc(
      s.shortcut
    )}"/></label><label>Хранить незакреплённые карточки, дней<input name="retention" type="number" min="1" max="3650" value="${
      s.retentionDays
    }"/></label><div class="notice">Исключение приложений подготовлено архитектурно, но отключено в MVP: X11 не сообщает надёжно источник Clipboard во всех случаях.</div><button class="primary" type="submit">Сохранить</button><button class="danger" type="button" data-clear>Очистить незакреплённую историю</button></form>`
  );
  if (!root) return;

  // 6.7: confirm before clearing all history.
  root.querySelector("[data-clear]")?.addEventListener("click", async () => {
    const confirmed = await confirmModal(
      "Очистить историю?",
      "Удалить всю незакреплённую историю? Это действие нельзя отменить."
    );
    if (confirmed) {
      try {
        await api.clear();
        await reload();
        root.innerHTML = "";
      } catch {
        showToast("Не удалось очистить историю");
      }
    }
  });

  root.querySelector("form")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const fd = new FormData(e.target as HTMLFormElement);
    const next = {
      ...s,
      paused: fd.get("recording") !== "on",
      autostart: fd.get("autostart") === "on",
      shortcut: String(fd.get("shortcut")),
      retentionDays: Number(fd.get("retention")),
    };
    try {
      await api.saveSettings(next);
      state.settings = next;
      root.innerHTML = "";
    } catch (err) {
      showToast(typeof err === "string" ? err : "Не удалось сохранить настройки");
    }
  });
}

// ── Reload / bootstrap ────────────────────────────────────────────────────────

async function reload() {
  const data = await api.bootstrap(popup);
  state.categories = data.categories;
  state.settings = data.settings;
  state.clips = await api.list(query());
  state.hasMore = !popup && state.clips.length === 60;
  renderFilters();
  renderCards();

  // 6.4: show invalid-settings warning only once per session (not on each reload).
  // Use consume_invalid_settings_warning to clear the flag on the backend.
  if (!invalidWarningConsumed && !popup) {
    const shouldWarn = await api.consumeInvalidSettingsWarning();
    if (shouldWarn) {
      invalidWarningConsumed = true;
      setTimeout(() => {
        modal(
          "Предупреждение",
          `<div class="notice" style="border-left-color:var(--coral);background:#fae2dc;color:#92463a;padding:12px;font-size:12px;line-height:1.5;">Файл настроек (settings.json) был повреждён или содержал недопустимые значения. Настройки были сброшены до безопасных значений (90 дней), а исходный файл сохранён как <b>settings.invalid.json</b>.</div>`
        );
      }, 100);
    }
  }
}

// ── Keyboard navigation ───────────────────────────────────────────────────────

document.addEventListener("keydown", (e) => {
  if (!popup) return;
  if (e.key === "Escape") {
    if (isTauri) getCurrentWindow().hide();
  } else if (e.key === "ArrowDown") {
    e.preventDefault();
    state.selected = Math.min(state.selected + 1, state.clips.length - 1);
    renderCards();
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    state.selected = Math.max(0, state.selected - 1);
    renderCards();
  } else if (e.key === "Enter" && document.activeElement?.id === "search" && state.clips[state.selected]) {
    const clip = state.clips[state.selected]!;
    const doIt = async () => {
      try {
        if (isTauri) {
          await api.copy(clip.id, true);
        } else {
          const content = await api.getClipContent(clip.id);
          await api.copy(clip.id, true, content);
        }
      } catch {
        showToast("Не удалось скопировать");
      }
    };
    doIt();
  }
});

// ── Init ──────────────────────────────────────────────────────────────────────

renderShell();
reload();

if (isTauri) {
  listen("clips-changed", refresh);
  listen("categories-changed", reload);
  listen("settings-changed", reload);
  if (!popup) {
    listen("open-settings", showSettings);
  } else {
    getCurrentWindow().onFocusChanged(({ payload }) => {
      if (!payload) {
        getCurrentWindow().hide();
      } else {
        reload();
        document.querySelector<HTMLInputElement>("#search")?.focus();
      }
    });
  }
}
