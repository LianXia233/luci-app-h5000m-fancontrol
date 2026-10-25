// Dev-only helper: basic gettext .po well-formedness check for environments
// without msgfmt. Not shipped with the package.
'use strict';
const fs = require('fs');
const text = fs.readFileSync(process.argv[2], 'utf8').replace(/\r\n/g, '\n');
const entries = text.split('\n\n').map(e => e.trim()).filter(Boolean);
let bad = 0;
// A single msgid followed by a msgstr; the msgstr may continue on
// additional quoted lines (the header entry uses that form).
const entryRe = /^msgid "((?:[^"\\]|\\.)*)"\nmsgstr "((?:[^"\\]|\\.)*)"(?:\n"(?:[^"\\]|\\.)*")*$/;
for (const entry of entries) {
	if (!entryRe.test(entry)) {
		bad++;
		console.log('BAD ENTRY:', JSON.stringify(entry.slice(0, 100)));
	}
}
const count = entries.length - 1; // minus header
console.log(`${count} entries checked, ${bad} malformed`);
process.exit(bad ? 1 : 0);
