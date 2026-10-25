// Dev-only helper: extract unique translatable strings from the view JS.
// Not shipped with the package.
const fs = require('fs');
const text = fs.readFileSync(process.argv[2], 'utf8');
const re = /_\(\s*'((?:[^'\\]|\\.)*)'\s*\)/g;
const seen = new Set();
let m;
while ((m = re.exec(text)) !== null) {
	let s;
	try { s = JSON.parse('"' + m[1] + '"'); } catch (e) { continue; }
	seen.add(s);
}
const out = Array.from(seen).sort((a, b) => a.localeCompare(b, 'en'));
fs.writeFileSync(process.argv[3], out.join('\n'));
console.log(out.length + ' unique strings');
