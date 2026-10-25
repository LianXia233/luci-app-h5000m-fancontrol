#!/usr/bin/env node
/*
 * Translation catalogue audit for the LuCI view.
 *
 * Every user visible string in the view goes through _('...'), so the gettext
 * catalogue has to carry an entry for each of them or the page silently falls
 * back to English. This script compares the view against
 * po/zh_Hans/h5000m-fancontrol.po and fails on anything missing or untranslated.
 *
 * `--list` prints the msgids the view uses, which is how the catalogue was
 * first seeded; `--emit` additionally prints a po skeleton for new entries.
 */

'use strict';

const fs = require('fs');
const path = require('path');

const repo = path.join(__dirname, '..');
const VIEW = path.join(repo, 'htdocs', 'luci-static', 'resources', 'view', 'h5000m', 'fancontrol.js');
const PO = path.join(repo, 'po', 'zh_Hans', 'h5000m-fancontrol.po');

const args = process.argv.slice(2);
const listOnly = args.includes('--list');

function unescape(text) {
	return text
		.replace(/\\n/g, '\n')
		.replace(/\\t/g, '\t')
		.replace(/\\'/g, "'")
		.replace(/\\"/g, '"')
		.replace(/\\\\/g, '\\');
}

/* Collect the argument of every _('...') call in the view. */
function extractMsgids(source) {
	const found = new Set();
	const pattern = /_\(\s*(['"])((?:\\.|(?!\1)[^\\])*)\1/g;
	let match;

	while ((match = pattern.exec(source)) !== null)
		found.add(unescape(match[2]));

	/*
	 * The option table and the label helpers build msgids from plain string
	 * values that are later wrapped in _(), so scan those too. They are the
	 * `label:`/`hint:`/`title:`/`choices:` payloads of the static tables.
	 */
	const tablePattern = /(?:label|hint|title):\s*(['"])((?:\\.|(?!\1)[^\\])*)\1/g;

	while ((match = tablePattern.exec(source)) !== null)
		found.add(unescape(match[2]));

	const choicePattern = /choices:\s*\[([\s\S]*?)\]/g;

	while ((match = choicePattern.exec(source)) !== null) {
		const itemPattern = /['"]([A-Za-z][^'"]*)['"]/g;
		let item;

		while ((item = itemPattern.exec(match[1])) !== null) {
			/* Skip the machine values, keep the human readable labels. */
			if (/^(auto|manual|kernel|max|cpu|silent|balanced|performance|custom|1|0)$/.test(item[1]))
				continue;

			found.add(item[1]);
		}
	}

	const tabPattern = /\{\s*id:\s*'[a-z]+',\s*label:\s*'([^']+)'/g;

	while ((match = tabPattern.exec(source)) !== null)
		found.add(unescape(match[1]));

	[...found].forEach((text) => {
		if (text === '')
			found.delete(text);
	});

	return [...found].sort();
}

/* Parse the msgid/msgstr pairs of a gettext file, including multi line forms. */
function parsePo(text) {
	const entries = new Map();
	const lines = text.split('\n');
	let current = null;
	let mode = null;

	function flush() {
		if (current && current.id !== null)
			entries.set(current.id, current.translation);

		current = null;
	}

	for (const raw of lines) {
		const line = raw.trimEnd();

		if (line.startsWith('msgid ')) {
			flush();
			mode = 'id';
			current = { id: unquoteLine(line.slice(6)), translation: null };
			current.header = unquoteLine(line.slice(6)) === '';

			continue;
		}

		if (line.startsWith('msgstr ')) {
			mode = 'translation';
			if (current)
				current.translation = unquoteLine(line.slice(7));

			continue;
		}

		if (/^"/.test(line)) {
			const piece = unquoteLine(line);

			if (mode === 'id' && current)
				current.id += piece;
			else if (mode === 'translation' && current)
				current.translation = (current.translation || '') + piece;

			continue;
		}

		if (line === '' && current && current.translation !== null)
			flush();
	}

	flush();
	entries.delete('');
	return entries;
}

function unquoteLine(text) {
	const trimmed = text.trim();

	if (!trimmed.startsWith('"'))
		return '';

	const parts = trimmed.match(/^"((?:\\.|[^"\\])*)"/);

	return parts ? unescape(parts[1]) : '';
}

function main() {
	const source = fs.readFileSync(VIEW, 'utf8');
	const msgids = extractMsgids(source);

	if (listOnly) {
		msgids.forEach((text) => console.log(text));
		return;
	}

	const catalogue = parsePo(fs.readFileSync(PO, 'utf8'));
	const missing = [];
	const untranslated = [];

	msgids.forEach((text) => {
		if (!catalogue.has(text))
			missing.push(text);
		else if (!String(catalogue.get(text)).trim())
			untranslated.push(text);
	});

	const unused = [...catalogue.keys()].filter((text) => msgids.indexOf(text) === -1);

	console.log(`${msgids.length} strings used by the view, ${catalogue.size} catalogue entries`);

	if (missing.length) {
		console.error(`\n${missing.length} string(s) have no catalogue entry:`);
		missing.forEach((text) => console.error(`  ${text}`));
	}

	if (untranslated.length) {
		console.error(`\n${untranslated.length} string(s) have an empty translation:`);
		untranslated.forEach((text) => console.error(`  ${text}`));
	}

	if (unused.length) {
		console.warn(`\n${unused.length} catalogue entry(ies) the view no longer uses (kept for upgrades):`);
		unused.slice(0, 12).forEach((text) => console.warn(`  ${text}`));
	}

	if (missing.length || untranslated.length) {
		if (args.includes('--emit')) {
			console.error('\n# skeleton for the missing entries');
			missing.concat(untranslated).forEach((text) => {
				console.error(`msgid "${text.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n" "\n')}"\nmsgstr ""\n`);
			});
		}

		process.exitCode = 1;
		return;
	}

	console.log('the catalogue covers every string the view can show');
}

main();
