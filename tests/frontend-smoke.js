#!/usr/bin/env node
/*
 * Frontend smoke check for view/h5000m/fancontrol.js.
 *
 * This is a deliberately browser-faithful stub, not a convenience one. The
 * lesson it exists to catch: an `E('svg', ...)` helper that special-cases the
 * namespace makes every chart "exist" while the browser puts them in the XHTML
 * namespace and paints nothing. So:
 *   - document.createElement() produces an XHTML-namespaced node, as the
 *     browser does for unknown tag names;
 *   - only document.createElementNS(SVG_NS, ...) yields a real SVG node;
 *   - assertions check namespaceURI and computed attribute values, never just
 *     "a tag with this name is present".
 *
 * Run: node tests/frontend-smoke.js
 */

'use strict';

const fs = require('fs');
const path = require('path');
const vm = require('vm');

const VIEW = path.join(__dirname, '..', 'htdocs', 'luci-static', 'resources', 'view', 'h5000m', 'fancontrol.js');
const SVG_NS = 'http://www.w3.org/2000/svg';
const HTML_NS = 'http://www.w3.org/1999/xhtml';

let failures = 0;
let checks = 0;

function check(label, condition, detail) {
	checks += 1;

	if (condition)
		return;

	failures += 1;
	console.error(`FAIL: ${label}${detail ? ' -> ' + detail : ''}`);
}

/* ------------------------------------------------------------------ DOM */

function makeElement(name, namespace) {
	const node = {
		nodeName: name,
		tagName: String(name).toUpperCase(),
		namespaceURI: namespace,
		attributes: {},
		children: [],
		style: {},
		ownText: '',
		classList: {
			add() {},
			contains() { return false; }
		},
		listeners: {},
	};

	Object.defineProperty(node, 'textContent', {
		get() { return textOf(node); },
		set(value) { node.children.length = 0; node.ownText = String(value); },
	});

	node.setAttribute = function(key, value) {
		node.attributes[key] = String(value);

		if (key === 'id') {
			node.id = String(value);
			/*
			 * Stub deviation, on purpose: a browser only resolves ids once the
			 * node is attached to the document. Nothing in this view queries an
			 * element before it is rendered, so registering eagerly matches what
			 * the page does without pretending to be a layout engine.
			 */
			byId[node.id] = node;
		}

		if (key === 'class')
			node.className = String(value);
	};

	node.getAttribute = function(key) {
		return Object.prototype.hasOwnProperty.call(node.attributes, key) ? node.attributes[key] : null;
	};

	node.appendChild = function(child) {
		if (typeof child === 'string')
			child = makeTextNode(child);

		if (child.parentNode && child.parentNode !== node)
			movedNodes.push(`${describe(child.parentNode)} -> ${describe(child)}`);

		node.children.push(child);
		child.parentNode = node;
		return child;
	};

	node.removeChild = function(child) {
		node.children = node.children.filter((item) => item !== child);

		if (child.parentNode === node)
			child.parentNode = null;

		return child;
	};

	node.addEventListener = function(type, handler) {
		node.listeners[type] = node.listeners[type] || [];
		node.listeners[type].push(handler);
	};

	node.dispatch = function(type, event) {
		(node.listeners[type] || []).forEach((handler) => handler(event || { target: node }));
	};

	Object.defineProperty(node, 'firstChild', {
		get() { return node.children.length ? node.children[0] : null; },
	});

	Object.defineProperty(node, 'lastChild', {
		get() { return node.children.length ? node.children[node.children.length - 1] : null; },
	});

	return node;
}

function makeTextNode(text) {
	const node = makeElement('#text', null);

	node.ownText = String(text);
	return node;
}

const byId = {};
/* Nodes the view appended to a second parent, which in a browser silently moves
 * them out of the first place they were rendered. */
const movedNodes = [];

function describe(node) {
	return `<${(node.tagName || node.nodeName || '?').toLowerCase()} class="${node.className || node.attributes.class || ''}" id="${node.id || ''}">`;
}

/* A browser's textContent concatenates the whole subtree. */
function textOf(node) {
	if (node.nodeName === '#text')
		return node.ownText;

	return node.ownText + node.children.map(textOf).join('');
}

const document = {
	activeElement: null,
	/* Browser-faithful: no SVG special case here on purpose. */
	createElement(name) { return makeElement(name, HTML_NS); },
	createElementNS(namespace, name) { return makeElement(name, namespace); },
	createTextNode(text) { return makeTextNode(text); },
	getElementId(id) { return byId[id] || null; },
	querySelector() { return null; },
	querySelectorAll() { return []; },
};

document.getElementById = document.getElementId;

function collect(node, into) {
	into = into || [];
	node.children.forEach((child) => {
		into.push(child);
		collect(child, into);
	});
	return into;
}

function findById(node, id) {
	if (node.id === id)
		return node;

	for (const child of collect(node)) {
		if (child.id === id)
			return child;
	}

	return null;
}

/* E() as LuCI's dom.js behaves for our purposes: createElement for everything,
 * attributes applied through setAttribute, arrays appended as children. */
function E(name, attrs, children) {
	const node = document.createElement(name);

	for (const key in (attrs || {})) {
		if (attrs[key] === null || attrs[key] === undefined || attrs[key] === false)
			continue;

		if (key === 'class')
			node.className = attrs[key];

		node.setAttribute(key, attrs[key] === true ? key : attrs[key]);
	}

	(children || []).forEach((child) => {
		if (child === null || child === undefined)
			return;

		node.appendChild(child);
	});

	return node;
}

const translations = new Map();

function _(text) {
	return String(text);
}

_.collect = function(text) {
	translations.set(String(text), true);
	return String(text);
};

/*
 * Only host-realm objects the view must share with the harness go into the
 * context. Realm intrinsics (String, Array, Promise, ...) are deliberately NOT
 * injected: string literals inside the VM belong to the VM's own String
 * prototype, and the format shim below has to patch that one.
 */
const sandboxGlobals = {
	console,
	document,
	E,
	_,
};

/*
 * LuCI installs String.prototype.format on the runtime's own String, and the
 * view's string literals belong to the VM realm, so the polyfill has to be
 * evaluated inside that realm rather than on this file's String.
 */
const FORMAT_SHIM = `
String.prototype.format = function() {
	var args = Array.prototype.slice.call(arguments), cursor = 0;

	return this.replace(/%(?:([0-9]+)\\$)?([sd%])/g, function(match, position, type) {
		if (type === '%')
			return '%';

		var index = position ? (parseInt(position, 10) - 1) : cursor++;
		return args[index] === undefined ? match : String(args[index]);
	});
};
`;

let formatCursor = 0;

const pollHandlers = [];
const executed = [];

const stubView = {
	extend(object) {
		const instance = Object.create(object);

		instance.renderFactory = () => object;
		executed.push(instance);
		return instance;
	},
};

const statusFixture = {
	schema: 3,
	generated_at: 1000,
	sample_mode: 'cached',
	controller: {
		version: '3.0.0', health: 'running', running: true, pid: 42, updated_at: 999,
		state_age_s: 1, stale: false, interval_s: 5, cycle_count: 7, consecutive_failures: 0,
		last_good_cycle: 999, last_error: null, result: 'ok', sensor_failure: false,
	},
	settings: {
		schema_version: 3, enabled: true, mode: 'auto', curve: 'balanced', temp_source: 'max',
		manual_pwm: 160, interval: 5, hysteresis: 2, down_delay: 30, start_pwm: 192,
		start_boost_ms: 700, fail_pwm: 255,
		curve_data: '20:0,35:0,40:50,55:60,70:75,85:90,95:100',
		curve_silent: '20:0,35:0,40:50,55:50,70:60,85:75,95:100',
		curve_balanced: '20:0,35:0,40:50,55:60,70:75,85:90,95:100',
		curve_performance: '20:0,30:60,45:70,60:82,75:92,85:100',
		curve_custom: '20:0,40:50,55:60,70:75,85:90,95:100',
		curve_valid: true, curve_error: null, kernel_only: false, issues: [],
		policy_signature: 'v1|enabled=true|mode=auto',
	},
	fan: {
		pwm_path: '/sys/class/hwmon/hwmon5/pwm1', pwm_value: 120, pwm_max: 255,
		requested_pwm: 102, target_pwm: 120, applied_pwm: 120,
		requested_percent: 40, target_percent: 47, applied_percent: 47,
		rpm: null, rpm_available: false, fan_path: null, reason: 'auto', reason_code: 'auto',
		kernel_floor_applied: false, boost_applied: false, down_since: 0, down_delay_remaining_s: 0,
	},
	thermal: {
		floor_pwm: 0, floor_percent: 0, state: 0, max_state: 3, owner: 'userspace-pwm',
		basis: 'conservative fallback', cooling_levels: [0, 64, 128, 255], map_states: [],
		legacy_policy_pending: false,
		protections: {
			zone: '/sys/class/thermal/thermal_zone0', type: 'cpu-thermal', policy: 'step_wise',
			available_policies: ['step_wise', 'user_space'],
			trips: [{ zone: 'cpu-thermal', name: 'critical', type: 'critical', temp_c: 115, hysteresis_c: 0 }],
			cpufreq_cooling: true, fan_cooling_device: '/sys/class/thermal/cooling_device0',
			fan_max_state: 3, intact: true, governor_hijacked: false, warnings: [],
		},
	},
	control: { temp_c: 61, sensor_label: 'mt799a_wiphy', sensor_source: 'hwmon:mt799a', source_mode: 'max' },
	sensors: [
		{ id: 'cpu', title: 'CPU', value_c: 52, status: 'ok', valid: true, label: 'cpu-thermal', source: 'thermal-zone:cpu-thermal', path: '/x', error: null, sampled_at: 999, age_s: 1 },
		{ id: 'phy', title: 'Ethernet PHY', value_c: 48, status: 'ok', valid: true, label: 'Phy-0', source: 'hwmon:Phy-0', path: '/x', error: null, sampled_at: 999, age_s: 1 },
		{ id: 'wifi1', title: 'Wi-Fi radio 1', value_c: 57, status: 'ok', valid: true, label: 'mt7996_wiphy', source: 'hwmon:mt7996_wiphy', path: '/x', error: null, sampled_at: 999, age_s: 1 },
		{ id: 'wifi2', title: 'Wi-Fi radio 2', value_c: 61, status: 'ok', valid: true, label: 'mt799a_wiphy', source: 'hwmon:mt799a_wiphy', path: '/x', error: null, sampled_at: 999, age_s: 1 },
		{ id: 'module', title: '5G modem', value_c: null, status: 'stale', valid: false, label: null, source: 'modem-cache', path: '/x', error: '90 s old', sampled_at: 900, age_s: 100 },
	],
	events: [
		{ ts: 990, level: 'warn', code: 'control', message: 'reason=sensor-failsafe' },
	],
	paths: {
		uci_config: '/etc/config/h5000m_fancontrol', state: '/var/run/h5000m-fancontrol.state',
		event_log: '/var/run/h5000m-fancontrol.events', lock_dir: '/var/lock/x', sysfs: '/sys',
	},
};

const uciStore = {
	enabled: '1', mode: 'auto', curve: 'balanced', temp_source: 'max', manual_pwm: '160',
	interval: '5', hysteresis: '2', down_delay: '30', start_pwm: '192', start_boost_ms: '700',
	fail_pwm: '255', curve_silent: statusFixture.settings.curve_silent,
	curve_balanced: statusFixture.settings.curve_balanced,
	curve_performance: statusFixture.settings.curve_performance,
	curve_custom: statusFixture.settings.curve_custom,
};

const modules = {
	view: stubView,
	fs: {
		exec(command, args) {
			executed.push({ command, args: args || [] });
			return Promise.resolve({ code: 0, stdout: JSON.stringify(statusFixture), stderr: '' });
		},
	},
	poll: { add(handler, interval) { pollHandlers.push({ handler, interval }); } },
	ui: {
		addNotification() {},
		showModal() {},
		hideModal() {},
	},
	uci: {
		load() { return Promise.resolve(); },
		unload() { return Promise.resolve(); },
		get(pkg, sid, key) {
			if (key === undefined)
				return uciStore.mode ? uciStore : null;

			return Object.prototype.hasOwnProperty.call(uciStore, key) ? uciStore[key] : null;
		},
		set(pkg, sid, key, value) { uciStore[key] = value; },
		add() {},
		save() { return Promise.resolve(); },
		apply() { return Promise.resolve(); },
	},
};

const L = {
	bind(fn, self) { return fn.bind(self); },
	url() { return '#'; },
};

const window = {
	location: { hash: '' },
	addEventListener() {},
	localStorage: {
		store: {},
		getItem(key) { return this.store[key] || null; },
		setItem(key, value) { this.store[key] = String(value); },
	},
};

sandboxGlobals.L = L;
sandboxGlobals.window = window;
sandboxGlobals.require = function(name) {
	if (!modules[name])
		throw new Error('unexpected require: ' + name);

	return modules[name];
};

const source = fs.readFileSync(VIEW, 'utf8');
const context = vm.createContext(sandboxGlobals);

vm.runInContext(FORMAT_SHIM, context, { filename: 'format-shim' });

/* LuCI turns each `'require x'` pragma into a module lookup; mirror that by
 * injecting the module names as parameters instead of editing the source. */
const exported = vm.runInContext(
	`(function(view, form, fs, poll, ui, uci, rpc, network, dom, require){\n${source}\n})`,
	context,
	{ filename: VIEW }
)(
	modules.view, modules.form, modules.fs, modules.poll, modules.ui, modules.uci,
	modules.rpc, modules.network, modules.dom,
	(name) => {
		if (!modules[name])
			throw new Error(`the view requires an unexpected module: ${name}`);

		return modules[name];
	}
);

const view = exported;

/* ------------------------------------------------------------- assertions */

function assertSvgTree(root, label) {
	const nodes = collect(root);
	const svgRoots = nodes.filter((node) => node.tagName === 'SVG');

	check(`${label}: at least one <svg> rendered`, svgRoots.length > 0, `found ${svgRoots.length}`);

	svgRoots.forEach((svg) => {
		check(`${label}: <svg> uses the SVG namespace`, svg.namespaceURI === SVG_NS,
			`namespaceURI=${svg.namespaceURI}`);
	});

	const descendants = [];
	svgRoots.forEach((svg) => collect(svg).forEach((node) => descendants.push(node)));

	['g', 'circle', 'path', 'text', 'line', 'rect', 'defs', 'linearGradient', 'stop'].forEach((name) => {
		const matched = descendants.filter((node) => node.tagName === name.toUpperCase());

		matched.forEach((node) => {
			check(`${label}: <${name}> inside <svg> is namespaced`, node.namespaceURI === SVG_NS,
				`namespaceURI=${node.namespaceURI}`);
		});
	});

	return { svgRoots, descendants };
}

async function main() {
	check('view exposes the lifecycle hooks',
		typeof view.load === 'function' && typeof view.render === 'function' && typeof view.paint === 'function');

	await view.load.call(view);

	check('status was fetched through the JSON interface',
		executed.some((call) => call.command === '/usr/sbin/h5000m-fancontrol-status'
			&& Array.isArray(call.args) && call.args[0] === 'json'),
		JSON.stringify(executed));

	check('config was seeded from uci, not from the status reply',
		view.values && view.values.mode === 'auto' && view.original.manual_pwm === '160');

	const root = await view.render.call(view);
	const nodes = collect(root);

	check('root node carries the plugin namespace',
		root.className === 'h5fan', String(root.className));

	const style = nodes.find((node) => node.tagName === 'STYLE');

	check('a scoped stylesheet is injected', !!style);
	check('the stylesheet never uses backdrop-filter',
		!String(style.children.map((c) => c.textContent).join('')).includes('backdrop-filter'));
	check('the root pins its own stacking context',
		/\.h5fan\{[^}]*position:relative[^}]*z-index:0[^}]*isolation:isolate/.test(
			String(style.children.map((c) => c.textContent).join(''))));

	const tabs = nodes.filter((node) => node.attributes.role === 'tab');

	check('six tabs are rendered', tabs.length === 6, `found ${tabs.length}`);
	check('exactly one tab is selected on load',
		tabs.filter((tab) => tab.attributes['aria-selected'] === 'true').length === 1);

	const panes = nodes.filter((node) => node.attributes['data-h5tab'] && node.tagName === 'DIV'
		&& String(node.className).indexOf('h5fan-pane') === 0);

	check('one pane per tab exists', panes.length === 6, `found ${panes.length}`);
	check('only the active pane is visible',
		panes.filter((pane) => !pane.hidden).length === 1,
		`visible ${panes.filter((pane) => !pane.hidden).map((pane) => pane.attributes['data-h5tab']).join(',')}`);

	/* SVG charts must be real SVG nodes, not XHTML look-alikes. */
	const allSvg = collect(root).filter((node) => node.tagName === 'SVG');
	const byClass = (name) => allSvg.filter((svg) => String(svg.className || svg.attributes.class || '').indexOf(name) > -1);
	const fanSvg = byClass('h5fan-fan');
	const chartSvg = byClass('h5fan-chart');
	const gaugeSvg = byClass('h5fan-gauge');
	const barSvg = byClass('h5fan-bar');

	check('fan diagram, curve chart, two gauges and ten temperature bars are rendered',
		fanSvg.length === 1 && chartSvg.length === 1 && gaugeSvg.length === 2 && barSvg.length === 10,
		`fan=${fanSvg.length} chart=${chartSvg.length} gauge=${gaugeSvg.length} bar=${barSvg.length}`);
	check('no rendered node was moved into a second parent',
		movedNodes.length === 0, movedNodes.join('; '));
	assertSvgTree(root, 'rendered page');

	check('the curve chart has an accessible name', !!chartSvg[0].attributes['aria-label']);
	check('the fan diagram says it is not a measured speed',
		/animation|not a measured/i.test(JSON.stringify(collect(fanSvg[0]).map((n) => n.textContent))));

	const blades = collect(fanSvg[0]).filter((node) => node.tagName === 'PATH'
		&& String(node.attributes.class || '').indexOf('h5fan-blade') > -1);

	check('the fan rotor has seven blades', blades.length === 7, `found ${blades.length}`);
	check('every blade is a real SVG path',
		blades.every((blade) => blade.namespaceURI === SVG_NS && blade.attributes.d));

	const curvePaths = collect(chartSvg[0]).filter((node) => node.tagName === 'PATH' && node.attributes.d);

	check('the curve chart draws a real path',
		curvePaths.length >= 2 && curvePaths.some((p) => p.attributes.d.length > 200),
		`paths=${curvePaths.length}`);

	const chartTexts = collect(chartSvg[0]).filter((node) => node.tagName === 'TEXT')
		.map((node) => node.textContent);

	check('the curve chart labels the marker with temperature and output',
		chartTexts.some((text) => /61 °C · 66%/.test(text)), JSON.stringify(chartTexts));
	check('the curve chart labels every declared point',
		chartTexts.some((text) => /55°:60%/.test(text)), JSON.stringify(chartTexts));

	/* Unknown and unsupported values must not be dressed up. */
	const flatText = JSON.stringify(collect(root).map((node) => node.textContent));

	check('no RPM figure is invented when the board has no tachometer',
		!/RPM/.test(flatText), 'rpm_available is false, so no number may appear');
	check('the tachometer card states that the board has no feedback',
		/No feedback/.test(flatText));

	const moduleBar = barSvg[4];
	const moduleFill = collect(moduleBar).find((node) => node.tagName === 'RECT'
		&& String(node.attributes.class || '').indexOf('h5fan-bar-fill') > -1);
	const moduleHatch = collect(moduleBar).find((node) => node.tagName === 'RECT'
		&& String(node.attributes.class || '').indexOf('h5fan-bar-hatched') > -1);

	check('a stale sensor draws no value bar', moduleFill && moduleFill.attributes.width === '0',
		JSON.stringify(moduleFill && moduleFill.attributes));
	check('a stale sensor is drawn as a hatched, unknown range',
		moduleHatch && moduleHatch.attributes.display === 'inline');

	const cpuFill = collect(barSvg[0]).find((node) => node.tagName === 'RECT'
		&& String(node.attributes.class || '').indexOf('h5fan-bar-fill') > -1);

	check('a valid sensor draws a proportional bar',
		cpuFill && parseFloat(cpuFill.attributes.width) > 0 && parseFloat(cpuFill.attributes.width) < 216,
		JSON.stringify(cpuFill && cpuFill.attributes));

	/* Interpolation must agree with the Rust backend to the integer. */
	check('the marker text matches the backend interpolation (61 C -> 66%)',
		chartTexts.some((text) => /66%/.test(text)));

	/* Validation must block out-of-range input before it reaches uci. */
	view.values = Object.assign({}, view.original);
	view.widgetInput = view.widgetInput || {};
	view.widgetError = {};

	Object.keys(view.values).forEach((key) => {
		view.widgetInput[key] = { setAttribute() {} };
	});

	check('interval above the maximum is rejected',
		typeof view.validateOption === 'function' && !!(() => {
			view.values.interval = '900';
			return view.validateOption('interval', false);
		})(), 'expected a message for interval=900');

	view.values.interval = '5';
	check('interval 5 is accepted', view.validateOption('interval', false) === null);

	view.values.curve_custom = '50:100,40:80';
	check('a non-increasing custom curve is rejected',
		!!view.validateOption('curve_custom', false), 'expected a message');

	view.values.curve_custom = '20:0,40:50';
	check('a two point custom curve is accepted', view.validateOption('curve_custom', false) === null);

	view.values.manual_pwm = '300';
	check('manual pwm beyond 255 is rejected', !!view.validateOption('manual_pwm', false));

	/* Saving must not touch the controller when only non-policy values moved. */
	view.original = Object.assign({}, view.values);
	view.values.fail_pwm = '250';
	check('a policy option change is detected as policy relevant',
		view.policyChanged(view.dirtyKeys()) === true);

	view.original = Object.assign({}, view.values);
	check('an unchanged form reports no dirty keys', view.dirtyKeys().length === 0);

	/* Failsafe health must be visible. */
	statusFixture.controller.health = 'failsafe';
	view.status = JSON.parse(JSON.stringify(statusFixture));
	view.paint.call(view);

	const repainted = JSON.stringify(collect(root).map((node) => node.textContent));

	check('the failsafe state is surfaced in the header',
		/Failsafe cooling active/.test(repainted), 'health text missing');

	/* Tab switching keeps state and records the choice. */
	view.selectTab('diagnostics');
	check('the pane follows the selected tab',
		panes.filter((pane) => !pane.hidden).map((pane) => pane.attributes['data-h5tab']).join(',') === 'diagnostics');
	check('the selected tab is remembered', window.localStorage.getItem('luci.h5000m-fancontrol.tab') === 'diagnostics');
	check('the tab is reflected in the location hash', window.location.hash === '#diagnostics');

	/* Poll cadence is decoupled from the controller interval. */
	check('the page polls on its own cadence', pollHandlers.length === 1 && pollHandlers[0].interval === 5,
		JSON.stringify(pollHandlers.map((entry) => entry.interval)));

	console.log(`\n${checks - failures}/${checks} frontend checks passed`);

	if (failures) {
		console.error(`${failures} frontend check(s) failed`);
		process.exitCode = 1;
	}
}

main().catch((err) => {
	console.error(`frontend smoke check crashed: ${err && err.stack ? err.stack : err}`);
	process.exit(2);
});
