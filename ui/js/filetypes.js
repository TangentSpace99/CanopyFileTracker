// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// Maps file extensions to categories and colours. Used by the tree, the
// treemap and the file-types view, so a type looks the same everywhere.

// Known file types: [category name, hue on the colour wheel, extensions].
// The treemap and the file-types view both colour files from this table.
const CATEGORIES = [
  ['Video', 275, 'mp4 mkv avi mov wmv flv webm m4v mpg mpeg vob m2ts mts 3gp'],
  ['Audio', 325, 'mp3 flac wav ogg m4a aac wma opus aiff mid'],
  ['Images', 42, 'jpg jpeg png gif bmp tif tiff webp heic raw cr2 nef arw dng psd svg ico'],
  ['Archives', 22, 'zip rar 7z tar gz bz2 xz zst cab tgz jar'],
  ['Documents', 212, 'pdf doc docx xls xlsx ppt pptx odt ods txt rtf md epub csv'],
  ['Code', 152, 'js ts jsx tsx py rs c cpp h hpp cs java go rb php html css json xml yml yaml toml sh ps1 sql ipynb'],
  ['Programs', 2, 'exe dll sys msi so pyd lib a o obj pdb node ocx drv'],
  ['Disk images', 186, 'iso img vhd vhdx vmdk vdi wim esd dmg bin'],
  ['Data', 95, 'db sqlite mdb dat pak bak log idx cache ba2 bsa'],
];

// extension -> { name, hue }, built once from the table above.
const EXT_CAT = new Map();

for (const [name, hue, exts] of CATEGORIES) for (const e of exts.split(' ')) EXT_CAT.set(e, { name, hue });

// extension -> computed style, so each extension is worked out only once.
const extCache = new Map();

/** Extension of a file name in lower case, or '' if it has none. Mirrors `extension()` in query.rs. */
export function extOf(name) {
  const i = name.lastIndexOf('.');
  if (i <= 0 || i === name.length - 1) return '';
  const e = name.slice(i + 1);
  return e.length <= 10 && /^[\w-]+$/.test(e) ? e.toLowerCase() : '';
}

/** Stable colour for an extension: known categories share a hue, the rest are muted. */
export function extStyle(ext) {
  let st = extCache.get(ext);
  if (st) return st;
  // A small string hash. It varies the shade within a category and picks the hue
  // of unknown extensions, so an extension always gets the same colour.
  let hash = 0;
  for (let i = 0; i < ext.length; i++) hash = (hash * 31 + ext.charCodeAt(i)) >>> 0;
  const cat = EXT_CAT.get(ext);
  st = cat
    ? { cat: cat.name, h: cat.hue + (hash % 5 - 2) * 3, s: 58, l: 52 + (hash % 3 - 1) * 4 }
    : { cat: ext ? 'Other' : 'No extension', h: ext ? hash % 360 : 215, s: ext ? 30 : 16, l: 48 };
  st.css = `hsl(${st.h} ${st.s}% ${st.l}%)`;
  extCache.set(ext, st);
  return st;
}
