import fs from "node:fs/promises";
import path from "node:path";
import sharp from "sharp";

// แก้ path ตรงนี้ให้ตรงกับไฟล์ APP_MENUS ของโปรเจกต์
import { APP_MENUS, type AppMenuGroup, type AppMenuItem } from "./menuItems";

const OUT_DIR = path.resolve("docs/menu-png");

// A4 @ 300PPI
const PAGE_W = 2480;
const PAGE_H = 3508;

const MARGIN_X = 180;
const MARGIN_TOP = 170;
const MARGIN_BOTTOM = 170;

const CONTENT_W = PAGE_W - MARGIN_X * 2;
const FOOTER_Y = PAGE_H - 95;

const FONT = {
  title: 58,
  section: 42,
  item: 31,
  meta: 25,
  small: 23,
};

type Row =
  | {
      kind: "section";
      label: string;
    }
  | {
      kind: "item";
      depth: number;
      label: string;
      id: string;
      action?: string;
      accelerator?: string;
      icon?: string;
      checked?: boolean;
      danger?: boolean;
      disabled?: boolean;
      description?: string;
    }
  | {
      kind: "separator";
      depth: number;
    };

function esc(value: unknown): string {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function flattenMenus(groups: AppMenuGroup[]): Row[] {
  const rows: Row[] = [];

  for (const group of groups) {
    rows.push({
      kind: "section",
      label: group.label,
    });

    walkItems(group.children, rows, 0);
  }

  return rows;
}

function walkItems(items: AppMenuItem[], rows: Row[], depth: number) {
  for (const item of items) {
    if (item.type === "separator") {
      rows.push({ kind: "separator", depth });
      continue;
    }

    if (item.type === "submenu") {
      rows.push({
        kind: "item",
        depth,
        label: item.label,
        id: item.id,
        icon: item.icon,
        disabled: item.enabled === false,
        description: "Submenu",
      });

      walkItems(item.children, rows, depth + 1);
      continue;
    }

    rows.push({
      kind: "item",
      depth,
      label: item.label,
      id: item.id,
      action: item.action,
      accelerator: item.accelerator,
      icon: item.icon,
      checked: item.checked,
      danger: item.danger,
      disabled: item.enabled === false,
      description: item.description,
    });
  }
}

function estimateRowHeight(row: Row): number {
  if (row.kind === "section") return 112;
  if (row.kind === "separator") return 34;

  let h = 84;

  if (row.description) h += 34;
  if (row.action || row.id) h += 30;

  return h;
}

function paginate(rows: Row[]): Row[][] {
  const pages: Row[][] = [];
  let page: Row[] = [];
  let y = MARGIN_TOP + 120;

  for (const row of rows) {
    const h = estimateRowHeight(row);

    if (y + h > PAGE_H - MARGIN_BOTTOM - 120 && page.length > 0) {
      pages.push(page);
      page = [];
      y = MARGIN_TOP + 120;
    }

    page.push(row);
    y += h;
  }

  if (page.length > 0) pages.push(page);

  return pages;
}

function renderPage(rows: Row[], pageIndex: number, pageCount: number): string {
  let y = MARGIN_TOP;

  const parts: string[] = [];

  parts.push(`
<svg width="${PAGE_W}" height="${PAGE_H}" viewBox="0 0 ${PAGE_W} ${PAGE_H}" xmlns="http://www.w3.org/2000/svg">
  <rect width="${PAGE_W}" height="${PAGE_H}" fill="#101114"/>
  <rect x="${MARGIN_X - 50}" y="${MARGIN_TOP - 80}" width="${CONTENT_W + 100}" height="${PAGE_H - MARGIN_TOP - MARGIN_BOTTOM + 20}" rx="34" fill="#15171c" stroke="#2a2d35" stroke-width="2"/>

  <text x="${MARGIN_X}" y="${y}" fill="#f4f6fb" font-family="system-ui, sans-serif" font-size="${FONT.title}" font-weight="800">
    Futureboard Studio Menu Map
  </text>

  <text x="${MARGIN_X}" y="${y + 48}" fill="#8d94a6" font-family="system-ui, sans-serif" font-size="${FONT.small}">
    Generated documentation · A4 300PPI · Page ${pageIndex + 1} / ${pageCount}
  </text>
`);

  y += 140;

  for (const row of rows) {
    if (row.kind === "section") {
      parts.push(`
  <text x="${MARGIN_X}" y="${y}" fill="#dce7ff" font-family="system-ui, sans-serif" font-size="${FONT.section}" font-weight="800">
    ${esc(row.label)}
  </text>
  <line x1="${MARGIN_X}" y1="${y + 24}" x2="${PAGE_W - MARGIN_X}" y2="${y + 24}" stroke="#313744" stroke-width="2"/>
`);
      y += estimateRowHeight(row);
      continue;
    }

    if (row.kind === "separator") {
      const indent = row.depth * 44;
      parts.push(`
  <line x1="${MARGIN_X + indent}" y1="${y}" x2="${PAGE_W - MARGIN_X}" y2="${y}" stroke="#282c34" stroke-width="2"/>
`);
      y += estimateRowHeight(row);
      continue;
    }

    const h = estimateRowHeight(row);
    const indent = row.depth * 54;
    const x = MARGIN_X + indent;

    const bg = row.disabled ? "#17191e" : "#1c1f27";
    const stroke = row.danger ? "#6d3232" : "#2b303a";
    const labelColor = row.disabled
      ? "#626a79"
      : row.danger
        ? "#ff9b9b"
        : "#f1f3f8";
    const metaColor = row.disabled ? "#4f5664" : "#8e97aa";

    parts.push(`
  <rect x="${x}" y="${y - 44}" width="${PAGE_W - MARGIN_X - x}" height="${h - 16}" rx="18" fill="${bg}" stroke="${stroke}" stroke-width="2"/>

  <text x="${x + 28}" y="${y}" fill="${labelColor}" font-family="system-ui, sans-serif" font-size="${FONT.item}" font-weight="700">
    ${row.checked ? "✓ " : ""}${esc(row.label)}
  </text>
`);

    if (row.accelerator) {
      parts.push(`
  <rect x="${PAGE_W - MARGIN_X - 260}" y="${y - 42}" width="230" height="44" rx="12" fill="#111318" stroke="#343a46" stroke-width="2"/>
  <text x="${PAGE_W - MARGIN_X - 145}" y="${y - 12}" text-anchor="middle" fill="#c6ccda" font-family="system-ui, sans-serif" font-size="${FONT.meta}" font-weight="700">
    ${esc(row.accelerator)}
  </text>
`);
    }

    let metaY = y + 37;

    parts.push(`
  <text x="${x + 30}" y="${metaY}" fill="${metaColor}" font-family="system-ui, sans-serif" font-size="${FONT.meta}">
    id: ${esc(row.id)}
  </text>
`);

    if (row.action) {
      parts.push(`
  <text x="${x + 480}" y="${metaY}" fill="${metaColor}" font-family="system-ui, sans-serif" font-size="${FONT.meta}">
    action: ${esc(row.action)}
  </text>
`);
    }

    if (row.description) {
      metaY += 34;
      parts.push(`
  <text x="${x + 30}" y="${metaY}" fill="#737d91" font-family="system-ui, sans-serif" font-size="${FONT.meta}">
    ${esc(row.description)}
  </text>
`);
    }

    y += h;
  }

  parts.push(`
  <text x="${MARGIN_X}" y="${FOOTER_Y}" fill="#687084" font-family="system-ui, sans-serif" font-size="${FONT.small}">
    Futureboard Studio · Menu Documentation
  </text>

  <text x="${PAGE_W - MARGIN_X}" y="${FOOTER_Y}" text-anchor="end" fill="#687084" font-family="system-ui, sans-serif" font-size="${FONT.small}">
    ${pageIndex + 1} / ${pageCount}
  </text>
</svg>
`);

  return parts.join("\n");
}

async function main() {
  await fs.rm(OUT_DIR, { recursive: true, force: true });
  await fs.mkdir(OUT_DIR, { recursive: true });

  const rows = flattenMenus(APP_MENUS);
  const pages = paginate(rows);

  for (let i = 0; i < pages.length; i++) {
    const svg = renderPage(pages[i], i, pages.length);
    const fileName = `menu-doc-${String(i + 1).padStart(3, "0")}.png`;
    const outPath = path.join(OUT_DIR, fileName);

    await sharp(Buffer.from(svg))
      .png({
        compressionLevel: 9,
        adaptiveFiltering: true,
      })
      .toFile(outPath);

    console.log(`Generated ${outPath}`);
  }

  console.log(`Done: ${pages.length} page(s)`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
