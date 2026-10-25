'use strict';
'require view';
'require uci';
'require fs';
'require poll';
'require ui';

/* H5000M fan control UI.
 *
 * Tabs: overview / control / curve / monitor / settings / diagnostics.
 * Status comes from the Rust controller as JSON (/usr/sbin/h5000m-fancontrol-status
 * wraps `h5000m-fancontrol status --json`); the daemon refreshes a snapshot
 * every control interval so web polling does not re-query sensors itself.
 * Configuration is edited through LuCI's uci API; Save & Apply commits the
 * config, and one bounded `apply` run makes the change effective immediately
 * (the daemon also reloads UCI every cycle and on SIGHUP).
 */

var CSS = [
	'.h5fc{--h5fc-ok:#36c98f;--h5fc-blue:#55a8ff;--h5fc-amber:#f0aa46;--h5fc-red:#ef6262;--h5fc-violet:#9d7bff;color:var(--text-color-high,#222)}',
	'.h5fc h2{margin:0 0 4px;font-size:21px;line-height:1.3}',
	'.h5fc-sub{margin:0 0 14px;color:var(--text-color-medium,#666);font-size:13px}',
	/* header + health chip */
	'.h5fc-hero{display:flex;justify-content:space-between;align-items:flex-start;gap:16px;padding:2px 2px 13px;margin:0 0 14px;border-bottom:1px solid var(--border-color-low,#e8e8e8)}',
	'.h5fc-health{display:inline-flex;align-items:center;gap:7px;padding:5px 11px;border-radius:999px;font-size:12px;font-weight:600;white-space:nowrap;background:var(--background-color-high,#f5f5f5);color:var(--h5fc-ok)}',
	'.h5fc-health:before{content:"";width:7px;height:7px;border-radius:50%;background:currentColor}',
	'.h5fc-health.warn{color:var(--h5fc-amber)}.h5fc-health.fail{color:var(--h5fc-red)}.h5fc-health.unknown{color:var(--text-color-medium,#888)}',
	/* tab strip */
	'.h5fc-tabs{display:flex;gap:4px;margin:0 0 16px;padding:4px;border:1px solid var(--border-color-low,#e8e8e8);border-radius:12px;background:var(--background-color-high,#fff);overflow-x:auto;scrollbar-width:thin}',
	'.h5fc-tab{flex:0 0 auto;display:inline-flex;align-items:center;gap:6px;padding:8px 14px;border:0;border-radius:9px;background:transparent;color:var(--text-color-medium,#666);font-size:13px;font-weight:600;cursor:pointer;white-space:nowrap}',
	'.h5fc-tab:hover{background:rgba(127,127,127,.08)}',
	'.h5fc-tab[aria-selected="true"]{background:linear-gradient(160deg,rgba(54,201,143,.16),rgba(54,201,143,.06));color:var(--h5fc-ok);box-shadow:inset 0 0 0 1px rgba(54,201,143,.35)}',
	'.h5fc-tab .h5fc-dot{width:6px;height:6px;border-radius:50%;background:var(--h5fc-amber);display:none}',
	'.h5fc-tab[data-dirty="1"] .h5fc-dot{display:inline-block}',
	/* cards + grids */
	'.h5fc-grid{display:grid;gap:12px;margin-bottom:16px}',
	'.h5fc-grid.c4{grid-template-columns:repeat(4,minmax(150px,1fr))}',
	'.h5fc-grid.c2{grid-template-columns:repeat(2,minmax(200px,1fr))}',
	'.h5fc-grid.c3{grid-template-columns:repeat(3,minmax(160px,1fr))}',
	'.h5fc-card{position:relative;overflow:hidden;padding:14px;border:1px solid var(--border-color-medium,#d8d8d8);border-radius:12px;background:var(--background-color-high,#fff)}',
	'.h5fc-card.accent{border-color:rgba(54,201,143,.45);background:linear-gradient(160deg,rgba(54,201,143,.12),rgba(54,201,143,.03))}',
	'.h5fc-card-title{font-size:12px;font-weight:600;color:var(--text-color-medium,#666);margin-bottom:8px}',
	'.h5fc-big{font-size:22px;font-weight:680;line-height:1.2}',
	'.h5fc-hint{font-size:11px;color:var(--text-color-low,#888);margin-top:6px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}',
	'.h5fc-panel{margin:0 0 16px;padding:16px;border:1px solid var(--border-color-medium,#d8d8d8);border-radius:14px;background:var(--background-color-high,#fff)}',
	'.h5fc-panel>h3{margin:0 0 12px;font-size:15px}',
	'.h5fc-panel-head{display:flex;justify-content:space-between;align-items:center;gap:12px;margin-bottom:12px;flex-wrap:wrap}',
	'.h5fc-panel-head>h3{margin:0}',
	'.h5fc-badge{padding:4px 9px;border-radius:7px;background:rgba(85,168,255,.13);color:var(--h5fc-blue);font-size:11px;font-weight:600;white-space:nowrap}',
	'.h5fc-badge.warn{background:rgba(240,170,70,.14);color:var(--h5fc-amber)}',
	'.h5fc-badge.fail{background:rgba(239,98,98,.14);color:var(--h5fc-red)}',
	'.h5fc-badge.ok{background:rgba(54,201,143,.14);color:var(--h5fc-ok)}',
	'.h5fc-badge.mute{background:rgba(127,127,127,.1);color:var(--text-color-medium,#666)}',
	/* chips row */
	'.h5fc-chips{display:flex;flex-wrap:wrap;gap:8px}',
	'.h5fc-chip{display:inline-flex;align-items:center;gap:6px;padding:6px 10px;border-radius:8px;border:1px solid var(--border-color-low,#e0e0e0);background:rgba(127,127,127,.05);font-size:12px;color:var(--text-color-high,#333)}',
	'.h5fc-chip b{font-weight:650}',
	'.h5fc-note{margin-top:12px;padding:10px 12px;border-left:3px solid var(--h5fc-blue);background:rgba(85,168,255,.08);color:var(--text-color-medium,#555);font-size:12px;line-height:1.5;border-radius:0 8px 8px 0}',
	'.h5fc-note.warn{border-left-color:var(--h5fc-amber);background:rgba(240,170,70,.1)}',
	'.h5fc-note.fail{border-left-color:var(--h5fc-red);background:rgba(239,98,98,.1)}',
	/* controls */
	'.h5fc-field{display:grid;gap:6px;padding:12px;border:1px solid var(--border-color-low,#e4e4e4);border-radius:10px;background:rgba(127,127,127,.03)}',
	'.h5fc-label{font-size:13px;font-weight:600}',
	'.h5fc-label small{display:block;font-weight:400;color:var(--text-color-medium,#777);font-size:11px;margin-top:2px;line-height:1.45}',
	'.h5fc-input,.h5fc-select{width:100%;box-sizing:border-box;padding:8px 10px;border:1px solid var(--border-color-medium,#cfcfcf);border-radius:8px;background:var(--background-color-high,#fff);color:var(--text-color-high,#222);font-size:13px;font-family:inherit}',
	'.h5fc-input:focus,.h5fc-select:focus{outline:none;border-color:var(--h5fc-ok);box-shadow:0 0 0 2px rgba(54,201,143,.18)}',
	'.h5fc-input.err{border-color:var(--h5fc-red);box-shadow:0 0 0 2px rgba(239,98,98,.15)}',
	'.h5fc-field .h5fc-err{font-size:11px;color:var(--h5fc-red);display:none}',
	'.h5fc-field.invalid .h5fc-err{display:block}',
	'.h5fc-slider{display:flex;align-items:center;gap:10px}',
	'.h5fc-slider input[type=range]{flex:1;min-width:0;accent-color:var(--h5fc-ok)}',
	'.h5fc-slider .h5fc-input{width:86px;flex:0 0 86px;text-align:center}',
	/* switch */
	'.h5fc-switch{position:relative;display:inline-block;width:46px;height:26px;flex:0 0 auto}',
	'.h5fc-switch input{opacity:0;width:0;height:0;position:absolute}',
	'.h5fc-switch i{position:absolute;inset:0;border-radius:999px;background:rgba(127,127,127,.35);transition:background .18s}',
	'.h5fc-switch i:before{content:"";position:absolute;left:3px;top:3px;width:20px;height:20px;border-radius:50%;background:#fff;box-shadow:0 1px 3px rgba(0,0,0,.25);transition:transform .18s}',
	'.h5fc-switch input:checked+i{background:var(--h5fc-ok)}',
	'.h5fc-switch input:checked+i:before{transform:translateX(20px)}',
	'.h5fc-switch input:focus-visible+i{box-shadow:0 0 0 2px rgba(54,201,143,.35)}',
	/* segmented */
	'.h5fc-seg{display:flex;gap:4px;padding:4px;border:1px solid var(--border-color-low,#e4e4e4);border-radius:10px;background:rgba(127,127,127,.04);flex-wrap:wrap}',
	'.h5fc-seg label{flex:1 1 auto;min-width:72px;text-align:center;padding:7px 10px;border-radius:7px;font-size:12px;font-weight:600;color:var(--text-color-medium,#666);cursor:pointer;white-space:nowrap;border:1px solid transparent}',
	'.h5fc-seg input{position:absolute;opacity:0;pointer-events:none}',
	'.h5fc-seg input:checked+span,.h5fc-seg label.on{background:linear-gradient(160deg,rgba(54,201,143,.18),rgba(54,201,143,.07));color:var(--h5fc-ok);border-color:rgba(54,201,143,.4)}',
	'.h5fc-seg label:hover{background:rgba(127,127,127,.08)}',
	/* buttons */
	'.h5fc-btn{display:inline-flex;align-items:center;gap:6px;padding:8px 14px;border:1px solid var(--border-color-medium,#cfcfcf);border-radius:9px;background:var(--background-color-high,#fff);color:var(--text-color-high,#333);font-size:13px;font-weight:600;cursor:pointer;font-family:inherit}',
	'.h5fc-btn:hover{border-color:var(--h5fc-ok);color:var(--h5fc-ok)}',
	'.h5fc-btn.primary{border-color:transparent;background:linear-gradient(160deg,#36c98f,#2aa877);color:#fff}',
	'.h5fc-btn.primary:hover{filter:brightness(1.06)}',
	'.h5fc-btn.danger{border-color:rgba(239,98,98,.5);color:var(--h5fc-red)}',
	'.h5fc-btn:disabled{opacity:.45;cursor:not-allowed}',
	/* overview layout */
	'.h5fc-overview{display:grid;grid-template-columns:260px minmax(0,1fr);gap:14px;align-items:stretch}',
	'.h5fc-fanbox{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:10px;padding:18px 14px}',
	/* fan svg */
	'.h5fc-fan-blades{transform-origin:100px 100px;animation:h5fc-spin 2.4s linear infinite;animation-play-state:paused}',
	'.h5fc-fan-run .h5fc-fan-blades{animation-play-state:running}',
	'@keyframes h5fc-spin{to{transform:rotate(360deg)}}',
	'@media (prefers-reduced-motion: reduce){.h5fc-fan-blades{animation:none!important}}',
	/* monitor */
	'.h5fc-gauges{display:grid;grid-template-columns:repeat(auto-fill,minmax(210px,1fr));gap:12px}',
	'.h5fc-gauge{display:flex;flex-direction:column;align-items:center;gap:2px;padding:14px 12px 12px}',
	'.h5fc-gauge .h5fc-gauge-name{font-size:12px;font-weight:650;color:var(--text-color-medium,#666);display:flex;align-items:center;gap:6px}',
	'.h5fc-gauge .h5fc-gauge-src{font-size:10px;color:var(--text-color-low,#999);max-width:100%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}',
	'.h5fc-gauge.active{border-color:rgba(85,168,255,.55);box-shadow:0 0 0 1px rgba(85,168,255,.25)}',
	/* curve */
	'.h5fc-curve-layout{display:grid;grid-template-columns:minmax(0,1fr) 300px;gap:14px;align-items:start}',
	'.h5fc-curve-svg-wrap{border:1px solid var(--border-color-low,#e4e4e4);border-radius:12px;background:linear-gradient(160deg,rgba(127,127,127,.035),rgba(127,127,127,.015));padding:6px}',
	'.h5fc-curve-svg{width:100%;height:auto;display:block;touch-action:none}',
	'.h5fc-nodes{display:grid;gap:6px;max-height:340px;overflow:auto;padding-right:2px}',
	'.h5fc-node{display:grid;grid-template-columns:1fr 1fr 30px;gap:8px;align-items:center}',
	'.h5fc-node .h5fc-input{padding:6px 8px;font-size:12px;text-align:right}',
	'.h5fc-node button{border:0;background:rgba(239,98,98,.12);color:var(--h5fc-red);border-radius:7px;height:30px;cursor:pointer;font-weight:700}',
	'.h5fc-node button:disabled{opacity:.3;cursor:not-allowed}',
	/* diagnostics */
	'.h5fc-kv{display:grid;grid-template-columns:auto 1fr;gap:6px 14px;font-size:12.5px}',
	'.h5fc-kv dt{color:var(--text-color-medium,#666);white-space:nowrap}',
	'.h5fc-kv dd{margin:0;font-weight:600;word-break:break-all}',
	'.h5fc-pre{margin:8px 0 0;padding:10px;border-radius:8px;background:rgba(127,127,127,.07);border:1px solid var(--border-color-low,#e4e4e4);font-size:11px;line-height:1.5;max-height:260px;overflow:auto;white-space:pre-wrap;word-break:break-all;color:var(--text-color-high,#333)}',
	'.h5fc-conn-lost{display:none;margin:0 0 14px;padding:10px 12px;border-radius:10px;border:1px solid rgba(239,98,98,.4);background:rgba(239,98,98,.1);color:var(--h5fc-red);font-size:12.5px;font-weight:600}',
	'.h5fc-conn-lost.show{display:block}',
	/* responsive */
	'@media (max-width: 1000px){.h5fc-overview{grid-template-columns:220px minmax(0,1fr)}.h5fc-curve-layout{grid-template-columns:1fr}.h5fc-grid.c4{grid-template-columns:repeat(2,minmax(150px,1fr))}.h5fc-grid.c3{grid-template-columns:repeat(2,minmax(150px,1fr))}}',
	'@media (max-width: 760px){.h5fc-overview{grid-template-columns:1fr}.h5fc-grid.c2{grid-template-columns:1fr}.h5fc-grid.c3{grid-template-columns:1fr}.h5fc-hero{flex-direction:column;gap:8px}.h5fc-nodes{max-height:240px}}',
	'@media (max-width: 520px){.h5fc-grid.c4{grid-template-columns:1fr}.h5fc-tab{padding:8px 10px}.h5fc-slider{flex-wrap:wrap}.h5fc-slider input[type=range]{flex:1 1 100%}.h5fc-slider .h5fc-input{flex:1 1 100%;width:100%}}'
].join('');

/* ------------------------------------------------------------------ *
 * configuration model                                                *
 * ------------------------------------------------------------------ */

var CFG_NAME = 'h5000m_fancontrol';
var CFG_SID = 'settings';

var OPTIONS = {
	enabled: { kind: 'bool', def: '1' },
	mode: { kind: 'enum', def: 'auto', values: ['auto', 'manual', 'kernel'] },
	curve: { kind: 'enum', def: 'balanced', values: ['silent', 'balanced', 'performance', 'custom'] },
	temp_source: { kind: 'enum', def: 'max', values: ['max', 'cpu'] },
	manual_pwm: { kind: 'uint', def: '160', min: 0, max: 255 },
	interval: { kind: 'uint', def: '5', min: 2, max: 60, unit: 's' },
	hysteresis: { kind: 'uint', def: '2', min: 0, max: 10, unit: '\u00b0C' },
	down_delay: { kind: 'uint', def: '30', min: 0, max: 300, unit: 's' },
	start_pwm: { kind: 'uint', def: '192', min: 80, max: 255 },
	start_boost_ms: { kind: 'uint', def: '700', min: 0, max: 3000, unit: 'ms' },
	fail_pwm: { kind: 'uint', def: '255', min: 192, max: 255 },
	curve_silent: { kind: 'curve', def: '20:0,35:0,40:50,55:50,70:60,85:75,95:100' },
	curve_balanced: { kind: 'curve', def: '20:0,35:0,40:50,55:60,70:75,85:90,95:100' },
	curve_performance: { kind: 'curve', def: '20:0,30:60,45:70,60:82,75:92,85:100' },
	curve_custom: { kind: 'curve', def: '20:0,40:50,55:60,70:75,85:90,95:100' }
};

/* Temperature bands. CPU boundaries mirror the controller's own kernel
 * cooling-map floors (83/113\u00b0C); the other sensors use conservative
 * plugin reference values - clearly labelled as non-official in the UI. */
var BANDS_CPU = [
	{ max: 70, cls: 'ok', label: _('Normal') },
	{ max: 83, cls: 'warn', label: _('Attention') },
	{ max: 113, cls: 'hot', label: _('Hot') },
	{ max: Infinity, cls: 'crit', label: _('Critical') }
];
var BANDS_GENERIC = [
	{ max: 60, cls: 'ok', label: _('Normal') },
	{ max: 75, cls: 'warn', label: _('Attention') },
	{ max: 95, cls: 'hot', label: _('Hot') },
	{ max: Infinity, cls: 'crit', label: _('Critical') }
];

function bandOf(bands, temp) {
	if (temp === null || temp === undefined)
		return null;
	for (var i = 0; i < bands.length; i++)
		if (temp < bands[i].max)
			return bands[i];
	return bands[bands.length - 1];
}

function isUInt(s) {
	return /^\d+$/.test(String(s));
}

function parseCurve(text) {
	var points = [], pairs = String(text || '').split(','), i, m, t, p;
	for (i = 0; i < pairs.length; i++) {
		m = /^(\d+):(\d+)$/.exec(pairs[i].trim());
		if (!m)
			return null;
		t = +m[1];
		p = +m[2];
		if (t > 150 || p > 100)
			return null;
		if (points.length && t <= points[points.length - 1].temp)
			return null;
		points.push({ temp: t, percent: p });
	}
	return points.length ? points : null;
}

function validateOption(key, value) {
	var spec = OPTIONS[key];
	if (!spec)
		return true;
	if (spec.kind === 'bool')
		return (value === '0' || value === '1') ? true : _('Must be 0 or 1.');
	if (spec.kind === 'enum')
		return spec.values.indexOf(value) > -1 ? true : _('Invalid value.');
	if (spec.kind === 'uint') {
		if (!isUInt(value))
			return _('Please enter a non-negative integer.');
		var n = +value;
		if (n < spec.min || n > spec.max)
			return _('Allowed range: %d\u2013%d').format(spec.min, spec.max);
		return true;
	}
	if (spec.kind === 'curve') {
		if (!parseCurve(value))
			return _('Use comma-separated temperature:percent points, e.g. 35:0,45:50,70:80,90:100. Temperatures must rise, 0\u2013150\u00b0C, percentages 0\u2013100.');
		return true;
	}
	return true;
}

/* PWM output of a curve at a temperature, mirroring the controller's
 * integer interpolation. */
function curvePwm(temp, points) {
	var prev = null, i, pt, result;
	for (i = 0; i < points.length; i++) {
		pt = points[i];
		if (temp <= pt.temp) {
			if (!prev)
				result = pt.percent;
			else
				result = prev.percent + Math.trunc((temp - prev.temp) * (pt.percent - prev.percent) / (pt.temp - prev.temp));
			return Math.trunc(result * 255 / 100);
		}
		prev = pt;
	}
	return prev ? Math.trunc(prev.percent * 255 / 100) : null;
}

function fmtTemp(v) {
	return (v === null || v === undefined) ? '\u2014' : _('%d\u00b0C').format(v);
}

function fmtPwm(v) {
	return (v === null || v === undefined) ? '\u2014' : _('%d / 255 (%d%%)').format(v, Math.round(v * 100 / 255));
}

function clamp(v, min, max) {
	return Math.max(min, Math.min(max, v));
}

var SVG_NS = 'http://www.w3.org/2000/svg';

/* LuCI's E() creates HTML elements; SVG nodes need their own namespace. */
function svgEl(name, attrs, text) {
	var el = document.createElementNS(SVG_NS, name), key;
	attrs = attrs || {};
	for (key in attrs) {
		if (attrs[key] !== undefined && attrs[key] !== null)
			el.setAttribute(key, attrs[key]);
	}
	if (text !== undefined && text !== null)
		el.appendChild(document.createTextNode(String(text)));
	return el;
}

return view.extend({
	/* ----------------------------------------------------------- *
	 * data loading                                                *
	 * ----------------------------------------------------------- */

	load: function() {
		var self = this;
		return uci.load(CFG_NAME).then(function() {
			self.resetWorkingCopy();
			return self.fetchStatus();
		});
	},

	fetchStatus: function() {
		var self = this;
		return fs.exec('/usr/sbin/h5000m-fancontrol-status').then(function(res) {
			var data = null;
			if (res && res.code === 0 && res.stdout) {
				try {
					data = JSON.parse(res.stdout);
				}
				catch (e) {
					data = null;
				}
			}
			self.statusBroken = !data;
			return data;
		}, function() {
			self.statusBroken = true;
			return null;
		});
	},

	uciValue: function(key) {
		var v = uci.get(CFG_NAME, CFG_SID, key);
		return (v === undefined || v === null || v === '') ? OPTIONS[key].def : String(v);
	},

	resetWorkingCopy: function() {
		this.values = {};
		this.orig = {};
		for (var key in OPTIONS) {
			this.values[key] = this.uciValue(key);
			this.orig[key] = this.values[key];
		}
		this.updateDirtyDots();
	},

	dirtyKeys: function() {
		var dirty = [], key;
		for (key in OPTIONS)
			if (String(this.values[key]) !== String(this.orig[key]))
				dirty.push(key);
		return dirty;
	},

	setValue: function(key, value) {
		this.values[key] = String(value);
		this.updateDirtyDots();
	},

	updateDirtyDots: function() {
		var dirty = this.dirtyKeys().length > 0;
		var tabs = document.querySelectorAll('.h5fc-tab');
		for (var i = 0; i < tabs.length; i++)
			tabs[i].setAttribute('data-dirty', dirty ? '1' : '0');
	},

	/* ----------------------------------------------------------- *
	 * save / apply / reset (header buttons)                       *
	 * ----------------------------------------------------------- */

	validateAll: function() {
		var errors = [], key, res;
		for (key in OPTIONS) {
			res = validateOption(key, this.values[key]);
			if (res !== true)
				errors.push(key + ': ' + res);
		}
		return errors;
	},

	handleSaveApply: function(ev /*, mode */) {
		return this.commit(true);
	},

	handleSave: function(ev) {
		return this.commit(false);
	},

	handleReset: function(ev) {
		var self = this;
		return uci.unload(CFG_NAME).then(function() {
			return uci.load(CFG_NAME);
		}).then(function() {
			self.resetWorkingCopy();
			self.rebuildStaticWidgets();
			ui.addNotification(null, E('p', _('Unsaved changes have been discarded.')), 'info');
		});
	},

	commit: function(applyNow) {
		var self = this, errors = this.validateAll(), dirty = this.dirtyKeys();

		if (errors.length) {
			ui.addNotification(null, E('p', _('Cannot save the configuration:') + '<br>' + errors.join('<br>')), 'error');
			return Promise.reject(new Error(errors[0]));
		}

		if (!dirty.length) {
			if (applyNow)
				return self.runApply().then(function() {
					ui.addNotification(null, E('p', _('Configuration unchanged; control cycle executed.')));
				});
			return Promise.resolve();
		}

		var disableConfirmed = true;
		if (this.orig.enabled === '1' && this.values.enabled === '0')
			disableConfirmed = false;

		var manualLow = false;
		if (this.values.mode === 'manual' && isUInt(this.values.manual_pwm) && +this.values.manual_pwm < 80)
			manualLow = true;

		return this.confirmation(
			(!disableConfirmed ? _('The enhanced controller is about to be disabled. The fan will follow the kernel thermal policy only. Continue?') : null),
			(manualLow ? _('Manual PWM below 80 may be too low to keep the fan spinning. Continue?') : null)
		).then(function(confirmed) {
			if (!confirmed)
				return null;

			if (!uci.get(CFG_NAME, CFG_SID))
				uci.add(CFG_NAME, 'settings', CFG_SID);
			for (var key in OPTIONS)
				uci.set(CFG_NAME, CFG_SID, key, String(self.values[key]));
			return uci.save()
				.then(function() { return uci.apply(); })
				.then(function() {
					for (var key in OPTIONS)
						self.orig[key] = String(self.values[key]);
					self.updateDirtyDots();
					if (!applyNow) {
						ui.addNotification(null, E('p', _('Settings saved. The controller picks them up within one control interval.')));
						return null;
					}
					return self.runApply().then(function() {
						ui.addNotification(null, E('p', _('Cooling policy saved and applied.')));
					});
				});
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('Failed to apply cooling policy:') + ' ' + (err && err.message ? err.message : err)), 'error');
			throw err;
		});
	},

	runApply: function() {
		return fs.exec('/usr/sbin/h5000m-fancontrol', ['apply']).then(function(res) {
			if (res && res.code !== 0)
				throw new Error(res.stderr || _('controller returned an error'));
			return res;
		});
	},

	confirmation: function() {
		var messages = Array.prototype.slice.call(arguments).filter(Boolean);
		if (!messages.length)
			return Promise.resolve(true);
		return new Promise(function(resolve) {
			ui.showModal(_('Please confirm'), [
				E('div', { style: 'margin-bottom:12px;line-height:1.6' }, messages.map(function(m) { return E('p', m); })),
				E('div', { class: 'right' }, [
					E('button', {
						class: 'btn',
						click: function() { ui.hideModal(); resolve(false); }
					}, _('Cancel')),
					' ',
					E('button', {
						class: 'btn cbi-button-positive important',
						click: function() { ui.hideModal(); resolve(true); }
					}, _('Continue'))
				])
			]);
		});
	},

	/* ----------------------------------------------------------- *
	 * tab navigation                                              *
	 * ----------------------------------------------------------- */

	tabList: function() {
		return [
			{ id: 'overview', title: _('Overview') },
			{ id: 'control', title: _('Fan Control') },
			{ id: 'curve', title: _('Fan Curve') },
			{ id: 'monitor', title: _('Temperatures') },
			{ id: 'settings', title: _('System Settings') },
			{ id: 'diagnostics', title: _('Diagnostics') }
		];
	},

	selectTab: function(id, push) {
		var tabs = this.tabList(), valid = false, i;
		for (i = 0; i < tabs.length; i++)
			if (tabs[i].id === id)
				valid = true;
		if (!valid)
			id = 'overview';
		this.activeTab = id;
		var panels = document.querySelectorAll('.h5fc-panel-wrap');
		for (i = 0; i < panels.length; i++)
			panels[i].style.display = (panels[i].getAttribute('data-tab') === id) ? '' : 'none';
		var buttons = document.querySelectorAll('.h5fc-tab');
		for (i = 0; i < buttons.length; i++)
			buttons[i].setAttribute('aria-selected', (buttons[i].getAttribute('data-tab') === id) ? 'true' : 'false');
		try { window.sessionStorage.setItem('h5000m-fancontrol-tab', id); } catch (e) { }
		if (push !== false && window.location.hash !== '#fan-' + id)
			window.history.pushState(null, '', '#fan-' + id);
		if (id === 'curve' && this.curveDrawn) {
			this.curveDrawn = false;
			this.drawCurveChart();
		}
	},

	restoreTab: function() {
		var id = null, m = /^#fan-([a-z]+)$/.exec(window.location.hash || '');
		if (m)
			id = m[1];
		if (!id) {
			try { id = window.sessionStorage.getItem('h5000m-fancontrol-tab'); } catch (e) { }
		}
		this.selectTab(id || 'overview', false);
	},

	/* ----------------------------------------------------------- *
	 * rendering                                                   *
	 * ----------------------------------------------------------- */

	render: function(data) {
		var self = this;
		this.status = data;

		var root = E('div', { class: 'h5fc' }, [
			E('style', { type: 'text/css' }, CSS),
			E('div', { class: 'h5fc-hero' }, [
				E('div', {}, [
					E('h2', _('Cooling management')),
					E('p', { class: 'h5fc-sub' }, _('Live fan policy, temperature sensors and kernel safety protection for the H5000M.'))
				]),
				E('div', { class: 'h5fc-health unknown', id: 'h5fc-health', title: '' }, _('Checking\u2026'))
			]),
			E('div', { class: 'h5fc-conn-lost', id: 'h5fc-conn-lost' },
				_('Cannot read controller status. Check that the h5000m-fancontrol service is installed and running.')),
			this.renderTabStrip(),
			E('div', { id: 'h5fc-panels' }, [
				E('div', { class: 'h5fc-panel-wrap', 'data-tab': 'overview' }, this.renderOverview()),
				E('div', { class: 'h5fc-panel-wrap', 'data-tab': 'control' }, this.renderControl()),
				E('div', { class: 'h5fc-panel-wrap', 'data-tab': 'curve' }, this.renderCurveTab()),
				E('div', { class: 'h5fc-panel-wrap', 'data-tab': 'monitor' }, this.renderMonitor()),
				E('div', { class: 'h5fc-panel-wrap', 'data-tab': 'settings' }, this.renderSettings()),
				E('div', { class: 'h5fc-panel-wrap', 'data-tab': 'diagnostics' }, this.renderDiagnostics())
			])
		]);

		window.addEventListener('hashchange', function() {
			var m = /^#fan-([a-z]+)$/.exec(window.location.hash || '');
			if (m && m[1] !== self.activeTab)
				self.selectTab(m[1], false);
		});

		/* LuCI inserts the returned node after render() resolves, so all
		 * getElementById-based widget wiring has to run one tick later. */
		window.setTimeout(L.bind(function() {
			this.restoreTab();
			this.wireManualSlider();
			this.wireChartClick();
			this.rebuildStaticWidgets();
			this.updateStatus(this.status || data);
		}, this), 0);

		poll.add(L.bind(function() {
			return this.fetchStatus().then(L.bind(function(next) {
				this.status = next;
				this.updateStatus(next);
			}, this));
		}, this), 3);

		return root;
	},

	renderTabStrip: function() {
		var self = this;
		return E('div', { class: 'h5fc-tabs', role: 'tablist' }, this.tabList().map(function(tab) {
			return E('button', {
				class: 'h5fc-tab',
				'data-tab': tab.id,
				role: 'tab',
				type: 'button',
				'aria-selected': 'false',
				click: function() { self.selectTab(tab.id); }
			}, [ E('span', { class: 'h5fc-dot' }), tab.title ]);
		}));
	},

	/* ---- shared small builders ---- */

	card: function(id, title, opts) {
		opts = opts || {};
		return E('div', { class: 'h5fc-card' + (opts.accent ? ' accent' : '') }, [
			E('div', { class: 'h5fc-card-title' }, title),
			E('div', { class: 'h5fc-big', id: id + '-value' }, opts.initial || '\u2026'),
			E('div', { class: 'h5fc-hint', id: id + '-hint' }, '')
		]);
	},

	chip: function(id, label) {
		return E('span', { class: 'h5fc-chip', id: id }, [ label + ' ', E('b', { id: id + '-v' }, '\u2014') ]);
	},

	field: function(label, description, body, err) {
		return E('div', { class: 'h5fc-field' }, [
			E('div', { class: 'h5fc-label' }, [ label, description ? E('small', {}, description) : '' ]),
			body,
			E('div', { class: 'h5fc-err' }, err || '')
		]);
	},

	/* ---- overview tab ---- */

	renderOverview: function() {
		return E('div', {}, [
			E('div', { class: 'h5fc-overview' }, [
				E('div', { class: 'h5fc-card h5fc-fanbox', id: 'h5fc-fanbox' }, [ this.fanSvg() ]),
				E('div', { class: 'h5fc-grid', style: 'grid-template-columns:1fr;align-content:start' }, [
					E('div', { class: 'h5fc-grid c4' }, [
						this.card('h5fc-ov-mode', _('Operating mode')),
						this.card('h5fc-ov-target', _('Target output')),
						this.card('h5fc-ov-applied', _('Actual output')),
						this.card('h5fc-ov-control', _('Control temperature'))
					]),
					E('div', { class: 'h5fc-card' }, [
						E('div', { class: 'h5fc-card-title' }, _('Safety protection')),
						E('div', { class: 'h5fc-chips' }, [
							this.chip('h5fc-ch-health', _('Controller')),
							this.chip('h5fc-ch-floor', _('Kernel safety floor')),
							this.chip('h5fc-ch-delay', _('Speed-down delay')),
							this.chip('h5fc-ch-owner', _('Fan ownership')),
							this.chip('h5fc-ch-rpm', _('Fan speed'))
						])
					]),
					E('div', { class: 'h5fc-card' }, [
						E('div', { class: 'h5fc-card-title' }, _('Temperatures')),
						E('div', { class: 'h5fc-grid c4', style: 'margin:0' }, [
							this.card('h5fc-ov-cpu', _('CPU')),
							this.card('h5fc-ov-phy', _('Ethernet PHY')),
							this.card('h5fc-ov-wifi', _('Wi-Fi radios')),
							this.card('h5fc-ov-modem', _('5G modem'))
						])
					])
				])
			]),
			E('div', { class: 'h5fc-note', id: 'h5fc-ov-note' }, '')
		]);
	},

	fanSvg: function() {
		/* Decorative fan illustration. The rotation speed is NOT a
		 * measurement of the real RPM - it only signals that the fan is
		 * being driven. */
		var blades = [], i, a;
		for (i = 0; i < 7; i++) {
			a = (360 / 7) * i;
			blades.push(svgEl('path', {
				d: 'M100 100 C 90 78, 96 52, 118 40 C 122 58, 118 80, 106 98 Z',
				fill: 'currentColor',
				opacity: '.92',
				transform: 'rotate(%d 100 100)'.format(a)
			}));
		}
		var svg = svgEl('svg', {
			viewBox: '0 0 200 226',
			width: '180', height: '203',
			role: 'img',
			id: 'h5fc-fan-svg',
			'aria-label': _('Fan status illustration')
		});
		svg.appendChild(svgEl('circle', { cx: 100, cy: 100, r: 92, fill: 'none', stroke: 'currentColor', 'stroke-width': 3, opacity: .25 }));
		svg.appendChild(svgEl('circle', { cx: 100, cy: 100, r: 84, fill: 'none', stroke: 'currentColor', 'stroke-width': 6, opacity: .12, id: 'h5fc-fan-ring' }));
		var g = svgEl('g', { class: 'h5fc-fan-blades', id: 'h5fc-fan-blades' });
		blades.forEach(function(b) { g.appendChild(b); });
		svg.appendChild(g);
		svg.appendChild(svgEl('circle', { cx: 100, cy: 100, r: 22, fill: 'currentColor', opacity: .95 }));
		svg.appendChild(svgEl('circle', { cx: 100, cy: 100, r: 8, fill: '#fff', opacity: .9 }));
		svg.appendChild(svgEl('text', { x: 100, y: 214, 'text-anchor': 'middle', 'font-size': 17, 'font-weight': 700, fill: 'currentColor', id: 'h5fc-fan-label' }, '\u2014'));
		return svg;
	},

	/* ---- control tab ---- */

	renderControl: function() {
		var self = this;

		var enabledSwitch = E('label', { class: 'h5fc-switch' }, [
			E('input', {
				type: 'checkbox', id: 'h5fc-enabled',
				change: function(ev) {
					self.setValue('enabled', ev.target.checked ? '1' : '0');
					self.updateModeVisibility();
				}
			}),
			E('i', {})
		]);

		var modeSeg = this.segmented('h5fc-mode', 'mode', [
			['auto', _('Automatic')],
			['manual', _('Manual')],
			['kernel', _('Kernel protection only')]
		]);

		var sourceSeg = this.segmented('h5fc-source', 'temp_source', [
			['max', _('Hottest sensor')],
			['cpu', _('CPU only')]
		]);

		var manualSlider = E('div', { class: 'h5fc-slider', id: 'h5fc-manual-row' }, [
			E('input', { type: 'range', min: 0, max: 255, step: 1, id: 'h5fc-manual-range' }),
			E('input', { type: 'number', min: 0, max: 255, step: 1, class: 'h5fc-input', id: 'h5fc-manual-number' })
		]);

		return E('div', {}, [
			E('div', { class: 'h5fc-panel' }, [
				E('div', { class: 'h5fc-panel-head' }, [
					E('h3', _('Fan control')),
					E('span', { class: 'h5fc-badge', id: 'h5fc-ctl-live' }, '\u2014')
				]),
				E('div', { class: 'h5fc-grid c2' }, [
					this.field(_('Enable enhanced controller'),
						_('When disabled, the fan follows the kernel thermal protection level only.'),
						enabledSwitch),
					this.field(_('Operating mode'),
						_('Automatic follows the fan curve; Manual drives a fixed PWM; Kernel protection only leaves the fan to the kernel governor.'),
						modeSeg)
				]),
				E('div', { class: 'h5fc-grid c2' }, [
					this.field(_('Control temperature'),
						_('Hottest sensor considers CPU, Ethernet PHY, Wi-Fi radios and the 5G modem cache.'),
						sourceSeg, '', 'h5fc-source-field'),
					this.field(_('Manual PWM output'),
						_('Applied in manual mode. The kernel safety floor may raise the effective output above this value.'),
						manualSlider)
				])
			]),
			E('div', { class: 'h5fc-panel' }, [
				E('div', { class: 'h5fc-panel-head' }, [ E('h3', _('Live output')), E('span', {}) ]),
				E('div', { class: 'h5fc-grid c4' }, [
					this.card('h5fc-ctl-requested', _('Requested output')),
					this.card('h5fc-ctl-target', _('Target output')),
					this.card('h5fc-ctl-applied', _('Actual output')),
					this.card('h5fc-ctl-reason', _('Control reason'))
				]),
				E('div', { style: 'display:flex;gap:10px;margin-top:14px;flex-wrap:wrap' }, [
					E('button', {
						class: 'h5fc-btn primary', type: 'button',
						click: function() {
							self.runApply().then(function() {
								ui.addNotification(null, E('p', _('Control cycle executed.')));
							}, function(err) {
								ui.addNotification(null, E('p', _('Control cycle failed:') + ' ' + (err.message || err)), 'error');
							});
						}
					}, _('Run control cycle now')),
					E('button', {
						class: 'h5fc-btn', type: 'button',
						click: function() {
							self.confirmation(_('Restart the fan control service? The fan keeps its current output during the restart.')).then(function(ok) {
								if (!ok)
									return;
								return fs.exec('/etc/init.d/h5000m-fancontrol', ['restart']).then(function() {
									ui.addNotification(null, E('p', _('Service restarted.')));
								}, function(err) {
									ui.addNotification(null, E('p', _('Service restart failed:') + ' ' + (err.message || err)), 'error');
								});
							});
						}
					}, _('Restart service'))
				])
			])
		]);
	},

	segmented: function(id, key, options) {
		var self = this;
		return E('div', { class: 'h5fc-seg', id: id, role: 'radiogroup' }, options.map(function(opt) {
			var label = E('label', {}, [
				E('input', {
					type: 'radio', name: id, value: opt[0],
					change: function(ev) {
						if (ev.target.checked) {
							self.setValue(key, opt[0]);
							if (key === 'curve')
								self.onCurveProfileChange();
						}
					}
				}),
				E('span', {}, opt[1])
			]);
			return label;
		}));
	},

	/* ---- curve tab ---- */

	renderCurveTab: function() {
		var self = this;

		var profileSeg = this.segmented('h5fc-curve-profile', 'curve', [
			['silent', _('Quiet')],
			['balanced', _('Balanced')],
			['performance', _('Performance')],
			['custom', _('Custom')]
		]);

		var chartCard = E('div', { class: 'h5fc-curve-svg-wrap' }, [
			svgEl('svg', { class: 'h5fc-curve-svg', id: 'h5fc-curve-svg', viewBox: '0 0 680 340', role: 'img' })
		]);

		this.curveEditorBuilt = false;

		var editorCard = E('div', { class: 'h5fc-card', id: 'h5fc-curve-editor' }, [
			E('div', { class: 'h5fc-card-title' }, _('Custom curve nodes')),
			E('div', { class: 'h5fc-hint', style: 'white-space:normal;margin:0 0 10px' },
				_('Drag the dots in the chart or edit the table. Click an empty spot to add a node; double-click a node to remove it.')),
			E('div', { class: 'h5fc-nodes', id: 'h5fc-curve-nodes' }),
			E('div', { style: 'display:flex;gap:8px;margin-top:10px;flex-wrap:wrap' }, [
				E('button', {
					class: 'h5fc-btn', type: 'button',
					click: function() { self.addCurveNode(); }
				}, _('Add node')),
				E('button', {
					class: 'h5fc-btn danger', type: 'button',
					click: function() {
						self.confirmation(_('Restore the default custom curve? Your current custom points will be replaced.')).then(function(ok) {
							if (!ok)
								return;
							self.setValue('curve_custom', OPTIONS.curve_custom.def);
							self.rebuildCurveEditor();
						});
					}
				}, _('Restore default'))
			])
		]);

		return E('div', { class: 'h5fc-panel' }, [
			E('div', { class: 'h5fc-panel-head' }, [
				E('h3', _('Fan curve')),
				profileSeg
			]),
			E('div', { class: 'h5fc-curve-layout' }, [
				chartCard,
				E('div', { style: 'display:grid;gap:12px' }, [
					E('div', { class: 'h5fc-grid c2', style: 'margin:0' }, [
						this.card('h5fc-curve-now', _('Operating point')),
						this.card('h5fc-curve-floor', _('Kernel floor'))
					]),
					editorCard
				])
			]),
			E('div', { class: 'h5fc-note', id: 'h5fc-curve-note' }, '')
		]);
	},

	onCurveProfileChange: function() {
		var editable = this.values.curve === 'custom';
		var editor = document.getElementById('h5fc-curve-editor');
		if (editor)
			editor.style.opacity = editable ? '1' : '.45';
		this.rebuildCurveEditor();
	},

	customPoints: function() {
		return parseCurve(this.values.curve_custom) || [];
	},

	syncCurveValue: function(points) {
		this.values.curve_custom = points.map(function(p) { return p.temp + ':' + p.percent; }).join(',');
		this.updateDirtyDots();
	},

	rebuildCurveEditor: function() {
		var self = this, holder = document.getElementById('h5fc-curve-nodes');
		if (!holder)
			return;
		var points = this.customPoints();
		holder.style.display = (this.values.curve === 'custom') ? '' : 'none';
		while (holder.firstChild)
			holder.removeChild(holder.firstChild);
		points.forEach(function(point, index) {
			var tInput = E('input', { type: 'number', min: 0, max: 150, step: 1, class: 'h5fc-input', value: point.temp });
			var pInput = E('input', { type: 'number', min: 0, max: 100, step: 1, class: 'h5fc-input', value: point.percent });
			var apply = function() {
				var pts = self.customPoints();
				if (!pts[index])
					return;
				pts[index] = { temp: clamp(parseInt(tInput.value, 10) || 0, 0, 150), percent: clamp(parseInt(pInput.value, 10) || 0, 0, 100) };
				var sorted = pts.slice().sort(function(a, b) { return a.temp - b.temp; });
				var ok = parseCurve(sorted.map(function(p) { return p.temp + ':' + p.percent; }).join(','));
				if (!ok) {
					tInput.classList.add('err');
					pInput.classList.add('err');
					return;
				}
				tInput.classList.remove('err');
				pInput.classList.remove('err');
				self.syncCurveValue(sorted);
				self.rebuildCurveEditor();
				self.drawCurveChart();
			};
			tInput.addEventListener('change', apply);
			pInput.addEventListener('change', apply);
			var del = E('button', { type: 'button', title: _('Delete node') }, '\u00d7');
			if (points.length <= 2)
				del.disabled = true;
			del.addEventListener('click', function() {
				var pts = self.customPoints();
				if (pts.length > 2) {
					pts.splice(index, 1);
					self.syncCurveValue(pts);
					self.rebuildCurveEditor();
					self.drawCurveChart();
				}
			});
			holder.appendChild(E('div', { class: 'h5fc-node' }, [
				tInput, pInput, del
			]));
		});
	},

	addCurveNode: function() {
		var pts = this.customPoints();
		if (pts.length >= 16) {
			ui.addNotification(null, E('p', _('At most 16 nodes are supported.')), 'warning');
			return;
		}
		var last = pts[pts.length - 1], temp = Math.min(150, (last ? last.temp : 30) + 5), percent = last ? last.percent : 50;
		pts.push({ temp: temp, percent: percent });
		pts.sort(function(a, b) { return a.temp - b.temp; });
		this.syncCurveValue(pts);
		this.rebuildCurveEditor();
		this.drawCurveChart();
	},

	/* Curve chart ------------------------------------------------- */

	chartGeom: function(points) {
		var minT = 20, maxT = 100, i;
		for (i = 0; i < points.length; i++) {
			minT = Math.min(minT, points[i].temp);
			maxT = Math.max(maxT, points[i].temp);
		}
		maxT = Math.max(maxT + 4, minT + 20);
		return {
			l: 46, r: 14, t: 16, b: 34, w: 680, h: 340,
			minT: minT, maxT: maxT
		};
	},

	drawCurveChart: function() {
		/* Cheap guard: the chart only matters on the curve tab, and a
		 * redraw during a node drag would break pointer capture. */
		if (this.draggingNode)
			return;
		if (this.activeTab && this.activeTab !== 'curve' && this.curveDrawn)
			return;

		var svg = document.getElementById('h5fc-curve-svg');
		if (!svg)
			return;
		var self = this;
		var profile = this.values.curve || 'balanced';
		var dataStr = (profile === 'custom') ? this.values.curve_custom : this.values['curve_' + profile];
		var points = parseCurve(dataStr) || [];
		var g = this.chartGeom(points);
		this.lastChartGeom = g;
		var plotW = g.w - g.l - g.r, plotH = g.h - g.t - g.b;
		var x = function(t) { return g.l + clamp((t - g.minT) / (g.maxT - g.minT), 0, 1) * plotW; };
		var y = function(p) { return g.t + (100 - clamp(p, 0, 100)) * plotH / 100; };

		while (svg.firstChild)
			svg.removeChild(svg.firstChild);
		this.curveDrawn = true;

		if (points.length < 2) {
			svg.setAttribute('aria-label', _('Invalid fan curve'));
			svg.appendChild(svgEl('text', { x: g.w / 2, y: g.h / 2, 'text-anchor': 'middle', 'font-size': 14, fill: 'var(--h5fc-red,#ef6262)' }, _('Invalid curve - check the node table')));
			return;
		}

		/* grid */
		var grid = svgEl('g', { fill: 'none' }), i, tick, step = g.maxT - g.minT > 70 ? 20 : 10;
		for (tick = Math.ceil(g.minT / step) * step; tick <= g.maxT; tick += step)
			grid.appendChild(svgEl('line', { x1: x(tick), y1: g.t, x2: x(tick), y2: g.t + plotH, stroke: 'currentColor', 'stroke-opacity': '.08' }));
		for (i = 0; i <= 100; i += 25)
			grid.appendChild(svgEl('line', { x1: g.l, y1: y(i), x2: g.l + plotW, y2: y(i), stroke: 'currentColor', 'stroke-opacity': '.1' }));
		svg.appendChild(grid);

		/* axis labels */
		var labels = svgEl('g', {});
		for (tick = Math.ceil(g.minT / step) * step; tick <= g.maxT; tick += step)
			if (tick % (step * 2) === 0 || step >= 20)
				labels.appendChild(svgEl('text', { x: x(tick), y: g.h - 12, 'text-anchor': 'middle', 'font-size': 11, fill: 'currentColor', opacity: .55 }, tick + '\u00b0'));
		for (i = 0; i <= 100; i += 25)
			labels.appendChild(svgEl('text', { x: g.l - 8, y: y(i) + 4, 'text-anchor': 'end', 'font-size': 11, fill: 'currentColor', opacity: .55 }, i + '%'));
		labels.appendChild(svgEl('text', { x: g.l + plotW / 2, y: g.h - 1, 'text-anchor': 'middle', 'font-size': 10, fill: 'currentColor', opacity: .45 }, _('Temperature')));
		svg.appendChild(labels);

		/* kernel floor reference */
		if (this.status && this.status.safety && this.status.safety.thermal_floor > 0) {
			var floorPct = Math.round(this.status.safety.thermal_floor * 100 / 255);
			var fg = svgEl('g', {});
			fg.appendChild(svgEl('line', { x1: g.l, y1: y(floorPct), x2: g.l + plotW, y2: y(floorPct), stroke: 'var(--h5fc-amber,#f0aa46)', 'stroke-dasharray': '6 5', 'stroke-width': 1.5, opacity: .8 }));
			fg.appendChild(svgEl('text', { x: g.l + plotW - 4, y: y(floorPct) - 5, 'text-anchor': 'end', 'font-size': 10, fill: 'var(--h5fc-amber,#f0aa46)' }, _('kernel floor %d%%').format(floorPct)));
			svg.appendChild(fg);
		}

		/* area + line */
		var d = 'M' + x(points[0].temp) + ' ' + y(points[0].percent);
		for (i = 1; i < points.length; i++)
			d += ' L' + x(points[i].temp) + ' ' + y(points[i].percent);
		var area = d + ' L' + x(points[points.length - 1].temp) + ' ' + (g.t + plotH) + ' L' + x(points[0].temp) + ' ' + (g.t + plotH) + ' Z';
		svg.appendChild(svgEl('path', { d: area, fill: 'var(--h5fc-ok,#36c98f)', opacity: .12 }));
		svg.appendChild(svgEl('path', { d: d, fill: 'none', stroke: 'var(--h5fc-ok,#36c98f)', 'stroke-width': 3.5, 'stroke-linejoin': 'round', 'stroke-linecap': 'round' }));

		/* nodes */
		var editable = profile === 'custom';
		points.forEach(function(point, index) {
			var c = svgEl('circle', {
				cx: x(point.temp), cy: y(point.percent),
				r: editable ? 7 : 5,
				fill: '#fff', stroke: 'var(--h5fc-ok,#36c98f)', 'stroke-width': 3,
				style: editable ? 'cursor:grab' : ''
			});
			if (editable) {
				self.attachNodeDrag(c, index, g);
				c.addEventListener('dblclick', function(ev) {
					ev.preventDefault();
					var pts = self.customPoints();
					if (pts.length > 2) {
						pts.splice(index, 1);
						self.syncCurveValue(pts);
						self.rebuildCurveEditor();
						self.drawCurveChart();
					}
				});
			}
			svg.appendChild(c);
		});

		/* current operating point marker */
		var st = this.status;
		if (st && st.sensors && st.sensors.control && st.sensors.control.temp_c !== null && st.controller && st.controller.mode === 'auto') {
			var ct = st.sensors.control.temp_c;
			var pct = Math.round((curvePwm(ct, points) || 0) * 100 / 255);
			var cx = x(ct);
			var mg = svgEl('g', {});
			mg.appendChild(svgEl('line', { x1: cx, y1: g.t, x2: cx, y2: g.t + plotH, stroke: 'var(--h5fc-blue,#55a8ff)', 'stroke-dasharray': '4 4', opacity: .7 }));
			mg.appendChild(svgEl('circle', { cx: cx, cy: y(pct), r: 6, fill: 'var(--h5fc-blue,#55a8ff)', stroke: '#fff', 'stroke-width': 2 }));
			mg.appendChild(svgEl('text', { x: clamp(cx + 8, g.l, g.l + plotW - 70), y: y(pct) - 9, 'font-size': 11, 'font-weight': 600, fill: 'var(--h5fc-blue,#55a8ff)' }, ct + '\u00b0C \u00b7 ' + pct + '%'));
			svg.appendChild(mg);
		}

		var aria = _('Fan curve: %d points from %d degrees at %d%% to %d degrees at %d%%')
			.format(points.length, points[0].temp, points[0].percent, points[points.length - 1].temp, points[points.length - 1].percent);
		svg.setAttribute('aria-label', aria);
	},

	wireChartClick: function() {
		/* Wired once; editability is checked at event time so profile
		 * switches do not pile up listeners. */
		var self = this;
		var svg = document.getElementById('h5fc-curve-svg');
		if (!svg || svg.dataset.h5fcClickWired)
			return;
		svg.dataset.h5fcClickWired = '1';
		svg.addEventListener('pointerdown', function(ev) {
			if (self.values.curve !== 'custom')
				return;
			if (ev.target.tagName === 'circle' || ev.target.tagName === 'text')
				return;
			var g = self.lastChartGeom;
			if (!g)
				return;
			var rect = svg.getBoundingClientRect();
			var plotW = g.w - g.l - g.r, plotH = g.h - g.t - g.b;
			var sx = (ev.clientX - rect.left) * g.w / rect.width;
			var sy = (ev.clientY - rect.top) * g.h / rect.height;
			if (sx < g.l || sx > g.l + plotW || sy < g.t || sy > g.t + plotH)
				return;
			var temp = clamp(Math.round(g.minT + (sx - g.l) / plotW * (g.maxT - g.minT)), 0, 150);
			var percent = clamp(Math.round(100 - (sy - g.t) * 100 / plotH), 0, 100);
			var pts = self.customPoints();
			if (pts.length >= 16)
				return;
			pts.push({ temp: temp, percent: percent });
			pts.sort(function(a, b) { return a.temp - b.temp; });
			if (parseCurve(pts.map(function(p) { return p.temp + ':' + p.percent; }).join(','))) {
				self.syncCurveValue(pts);
				self.rebuildCurveEditor();
				self.drawCurveChart();
			}
		});
	},

	attachNodeDrag: function(circle, index, g) {
		var self = this;
		circle.addEventListener('pointerdown', function(ev) {
			ev.preventDefault();
			var svg = document.getElementById('h5fc-curve-svg');
			var rect = svg.getBoundingClientRect();
			var plotW = g.w - g.l - g.r, plotH = g.h - g.t - g.b;
			var x = function(t) { return g.l + clamp((t - g.minT) / (g.maxT - g.minT), 0, 1) * plotW; };
			var y = function(p) { return g.t + (100 - clamp(p, 0, 100)) * plotH / 100; };
			circle.setPointerCapture(ev.pointerId);
			circle.style.cursor = 'grabbing';
			self.draggingNode = true;

			var move = function(e) {
				var pts = self.customPoints();
				if (!pts[index])
					return;
				var sx = (e.clientX - rect.left) * g.w / rect.width;
				var sy = (e.clientY - rect.top) * g.h / rect.height;
				/* keep at least 1 degree distance to the neighbours */
				var lo = index ? pts[index - 1].temp + 1 : 0;
				var hi = (index < pts.length - 1) ? pts[index + 1].temp - 1 : 150;
				var temp = clamp(Math.round(g.minT + (sx - g.l) / plotW * (g.maxT - g.minT)), lo, hi);
				var percent = clamp(Math.round(100 - (sy - g.t) * 100 / plotH), 0, 100);
				pts[index] = { temp: temp, percent: percent };
				circle.setAttribute('cx', x(temp));
				circle.setAttribute('cy', y(percent));
				self.pendingPoints = pts;
			};

			var up = function(e) {
				try { circle.releasePointerCapture(e.pointerId); } catch (err) { }
				circle.style.cursor = 'grab';
				window.removeEventListener('pointermove', move);
				window.removeEventListener('pointerup', up);
				self.draggingNode = false;
				if (self.pendingPoints) {
					var sorted = self.pendingPoints.slice().sort(function(a, b) { return a.temp - b.temp; });
					self.pendingPoints = null;
					self.syncCurveValue(sorted);
					self.rebuildCurveEditor();
					self.drawCurveChart();
				}
			};
			window.addEventListener('pointermove', move);
			window.addEventListener('pointerup', up);
		});
	},

	/* ---- monitor tab ---- */

	renderMonitor: function() {
		var self = this;
		var sensors = [
			{ id: 'cpu', name: _('CPU'), bands: BANDS_CPU, kernelAligned: true },
			{ id: 'phy', name: _('Ethernet PHY'), bands: BANDS_GENERIC },
			{ id: 'wifi1', name: _('Wi-Fi radio 1'), bands: BANDS_GENERIC },
			{ id: 'wifi2', name: _('Wi-Fi radio 2'), bands: BANDS_GENERIC },
			{ id: 'module', name: _('5G modem'), bands: BANDS_GENERIC }
		];
		return E('div', { class: 'h5fc-panel' }, [
			E('div', { class: 'h5fc-panel-head' }, [
				E('h3', _('Temperature monitoring')),
				E('span', { class: 'h5fc-badge', id: 'h5fc-mon-control' }, '\u2014')
			]),
			E('div', { class: 'h5fc-gauges' }, sensors.map(function(s) {
				return E('div', { class: 'h5fc-card h5fc-gauge', id: 'h5fc-gauge-' + s.id }, [
					E('div', { class: 'h5fc-gauge-name' }, [ s.name, E('span', { class: 'h5fc-badge mute', id: 'h5fc-gauge-' + s.id + '-band', style: 'font-size:10px;padding:2px 6px' }, '\u2014') ]),
					self.gaugeSvg('h5fc-gauge-' + s.id + '-svg'),
					E('div', { class: 'h5fc-gauge-src', id: 'h5fc-gauge-' + s.id + '-src' }, '\u2014')
				]);
			})),
			E('div', { class: 'h5fc-note' },
				_('Colour bands are plugin reference values. The CPU bands mirror the controller\u2019s kernel cooling-map floors (83/113\u00b0C); other sensors use conservative engineering estimates, not official vendor limits. A missing reading is shown as unknown, never as 0\u00b0C.'))
		]);
	},

	gaugeSvg: function(id) {
		/* 180 degree arc gauge, r=78, centre (100,96) */
		var a = function(pct) {
			var angle = Math.PI * (1 - clamp(pct, 0, 100) / 100);
			return [100 + 78 * Math.cos(angle), 96 - 78 * Math.sin(angle)];
		};
		var end = a(100);
		var svg = svgEl('svg', { viewBox: '0 0 200 112', width: '190', height: '106', id: id, role: 'img' });
		svg.appendChild(svgEl('path', { d: 'M22 96 A78 78 0 0 1 178 96', fill: 'none', stroke: 'currentColor', 'stroke-opacity': '.12', 'stroke-width': 13, 'stroke-linecap': 'round' }));
		svg.appendChild(svgEl('path', { d: 'M22 96 A78 78 0 0 1 ' + end[0].toFixed(1) + ' ' + end[1].toFixed(1), fill: 'none', stroke: 'var(--h5fc-ok,#36c98f)', 'stroke-width': 13, 'stroke-linecap': 'round', id: id + '-arc', opacity: .9 }));
		svg.appendChild(svgEl('text', { x: 100, y: 92, 'text-anchor': 'middle', 'font-size': 24, 'font-weight': 700, fill: 'currentColor', id: id + '-val' }, '\u2014'));
		svg.appendChild(svgEl('text', { x: 100, y: 107, 'text-anchor': 'middle', 'font-size': 10, fill: 'currentColor', opacity: .55 }, '\u00b0C'));
		return svg;
	},

	/* ---- settings tab ---- */

	renderSettings: function() {
		var self = this;
		var num = function(key, label, desc) {
			var spec = OPTIONS[key];
			var input = E('input', {
				type: 'number', min: spec.min, max: spec.max, step: 1,
				class: 'h5fc-input', id: 'h5fc-set-' + key, value: this.values[key]
			});
			var errEl = E('div', { class: 'h5fc-err' });
			input.addEventListener('input', function() {
				var res = validateOption(key, input.value);
				var ok = res === true;
				input.classList.toggle('err', !ok);
				errEl.textContent = ok ? '' : String(res);
				var fieldEl = input.closest('.h5fc-field');
				if (fieldEl)
					fieldEl.classList.toggle('invalid', !ok);
				if (ok)
					self.setValue(key, input.value);
			});
			var unit = spec.unit ? (' [' + spec.unit + ']') : '';
			return this.field(label + unit, desc + ' ' + _('Default: %s \u00b7 range %d\u2013%d.').format(spec.def, spec.min, spec.max), E('div', {}, [ input, errEl ]));
		};

		return E('div', { class: 'h5fc-panel' }, [
			E('div', { class: 'h5fc-panel-head' }, [
				E('h3', _('Response and safety parameters')),
				E('span', { class: 'h5fc-badge', id: 'h5fc-set-note' }, _('Changes apply after Save'))
			]),
			E('div', { class: 'h5fc-grid c3' }, [
				num.call(this, 'interval', _('Control interval'), _('Seconds between control decisions. The dashboard refreshes independently.')),
				num.call(this, 'hysteresis', _('Temperature hysteresis'), _('Prevents repeated speed changes around a curve point.')),
				num.call(this, 'down_delay', _('Speed-down delay'), _('Wait this many seconds before lowering fan output; increases apply immediately.')),
				num.call(this, 'start_pwm', _('Startup boost PWM'), _('Kick-off pulse used when the fan restarts from standstill.')),
				num.call(this, 'start_boost_ms', _('Startup boost duration'), _('A short boost helps a stopped fan start reliably. Set 0 to disable.')),
				num.call(this, 'fail_pwm', _('Sensor-failure PWM'), _('Output used when no valid sensor reading or an invalid curve is detected.'))
			]),
			E('div', { class: 'h5fc-note' },
				_('These parameters never bypass kernel protection: CPU throttling, hot and critical trips stay under kernel control in every mode.'))
		]);
	},

	/* ---- diagnostics tab ---- */

	renderDiagnostics: function() {
		var self = this;
		return E('div', { class: 'h5fc-panel' }, [
			E('div', { class: 'h5fc-panel-head' }, [
				E('h3', _('Diagnostics')),
				E('span', { class: 'h5fc-badge', id: 'h5fc-diag-health' }, '\u2014')
			]),
			E('div', { class: 'h5fc-grid c2' }, [
				E('div', { class: 'h5fc-card' }, [
					E('div', { class: 'h5fc-card-title' }, _('Controller')),
					E('dl', { class: 'h5fc-kv', id: 'h5fc-diag-ctl' })
				]),
				E('div', { class: 'h5fc-card' }, [
					E('div', { class: 'h5fc-card-title' }, _('Kernel thermal protection')),
					E('dl', { class: 'h5fc-kv', id: 'h5fc-diag-safety' })
				])
			]),
			E('div', { class: 'h5fc-card', style: 'margin-top:12px' }, [
				E('div', { class: 'h5fc-card-title' }, _('Configuration check')),
				E('div', { id: 'h5fc-diag-cfg', class: 'h5fc-hint', style: 'white-space:normal' } , '\u2014')
			]),
			E('details', { style: 'margin-top:12px' }, [
				E('summary', { style: 'cursor:pointer;font-size:13px;font-weight:600' }, _('Raw controller status (JSON)')),
				E('pre', { class: 'h5fc-pre', id: 'h5fc-diag-raw' }, '\u2014')
			])
		]);
	},

	/* ----------------------------------------------------------- *
	 * live status updates                                         *
	 * ----------------------------------------------------------- */

	reasonName: function(reason) {
		var kernelFloor = (reason || '').indexOf('+kernel-floor') > -1;
		var base = (reason || '').replace('+kernel-floor', '');
		var text;
		if (base === 'auto-down-delay') text = _('Waiting before speed down');
		else if (base === 'auto') text = _('Automatic curve');
		else if (base === 'manual') text = _('Manual request');
		else if (base === 'kernel') text = _('Kernel protection');
		else if (base === 'sensor-failsafe') text = _('Sensor failsafe');
		else if (base === 'curve-failsafe') text = _('Curve failsafe');
		else text = base || '\u2014';
		return kernelFloor ? _('%s \u00b7 kernel safety floor').format(text) : text;
	},

	modeName: function(mode) {
		if (mode === 'manual') return _('Manual');
		if (mode === 'kernel') return _('Kernel protection only');
		return _('Automatic');
	},

	profileName: function(p) {
		if (p === 'silent') return _('Quiet');
		if (p === 'performance') return _('Performance');
		if (p === 'custom') return _('Custom');
		return _('Balanced');
	},

	ownerName: function(owner) {
		if (owner === 'shared') return _('Shared with kernel');
		if (owner === 'userspace' || owner === 'userspace-pwm') return _('Userspace (kernel CPU throttling intact)');
		if (owner === 'kernel') return _('Kernel only');
		return _('Unknown');
	},

	sourceName: function(src) {
		if (src === 'thermal-zone') return _('Native thermal zone');
		if (src === 'hwmon') return _('Native hwmon');
		if (src === 'vendor-iwpriv') return _('Vendor driver (iwpriv)');
		if (src === 'modem-cache') return _('Modem manager cache');
		if (src === 'legacy-cache') return _('Legacy modem cache');
		return _('Not available');
	},

	setText: function(id, text) {
		var node = document.getElementById(id);
		if (node)
			node.textContent = text;
	},

	setHtml: function(id, html) {
		var node = document.getElementById(id);
		if (node)
			node.innerHTML = html;
	},

	fillKv: function(id, pairs) {
		var node = document.getElementById(id);
		if (!node)
			return;
		while (node.firstChild)
			node.removeChild(node.firstChild);
		pairs.forEach(function(pair) {
			node.appendChild(E('dt', {}, pair[0]));
			node.appendChild(E('dd', {}, pair[1] === undefined || pair[1] === null || pair[1] === '' ? '\u2014' : String(pair[1])));
		});
	},

	updateStatus: function(st) {
		var connLost = document.getElementById('h5fc-conn-lost');
		if (connLost)
			connLost.classList.toggle('show', !st);
		if (!st)
			return;

		var ctl = st.controller || {};
		var sensors = st.sensors || {};
		var fan = st.fan || {};
		var safety = st.safety || {};
		var effectiveMode = this.values.mode || ctl.mode;
		var effectiveEnabled = this.values.enabled !== '0';
		var effectiveCurve = this.values.curve || ctl.curve;

		/* health chip */
		var health = document.getElementById('h5fc-health');
		if (health) {
			var h = ctl.health || 'unknown';
			health.className = 'h5fc-health' + (h === 'ok' ? '' : ' ' + h);
			health.textContent = h === 'ok' ? _('Running normally')
				: h === 'warn' ? _('Status delayed')
				: h === 'fail' ? (ctl.result === 'failsafe' ? _('Failsafe cooling active') : _('Controller error'))
				: _('No data yet');
		}

		/* fan widget */
		this.updateFanWidget(fan, ctl, safety);

		/* overview cards */
		var controlTemp = sensors.control ? sensors.control.temp_c : null;
		this.setText('h5fc-ov-mode-value', effectiveEnabled ? this.modeName(effectiveMode) : _('Controller disabled'));
		this.setText('h5fc-ov-mode-hint', effectiveEnabled && effectiveMode === 'auto' ? this.profileName(effectiveCurve) : '');
		this.setText('h5fc-ov-target-value', fan.target_pwm !== null && fan.target_pwm !== undefined ? fmtPwm(fan.target_pwm) : '\u2014');
		this.setText('h5fc-ov-target-hint', _('requested: %s').format(fan.requested_pwm !== null && fan.requested_pwm !== undefined ? fmtPwm(fan.requested_pwm) : '\u2014'));
		this.setText('h5fc-ov-applied-value', fan.applied_pwm !== null && fan.applied_pwm !== undefined ? fmtPwm(fan.applied_pwm) : (fan.pwm !== null && fan.pwm !== undefined ? fmtPwm(fan.pwm) : '\u2014'));
		this.setText('h5fc-ov-applied-hint', fan.pwm_path ? fan.pwm_path : '');
		this.setText('h5fc-ov-control-value', fmtTemp(controlTemp));
		this.setText('h5fc-ov-control-hint', sensors.control && sensors.control.label ? sensors.control.label : _('No valid sensor'));

		var self = this;
		var tempCard = function(id, sensor) {
			self.setText(id + '-value', sensor && sensor.temp_c !== null && sensor.temp_c !== undefined ? fmtTemp(sensor.temp_c) : '\u2014');
			self.setText(id + '-hint', sensor && sensor.label ? sensor.label : '');
		};
		tempCard('h5fc-ov-cpu', sensors.cpu);
		tempCard('h5fc-ov-phy', sensors.phy);
		var wifiTemps = [sensors.wifi1, sensors.wifi2].filter(function(s) { return s && s.temp_c !== null && s.temp_c !== undefined; });
		this.setText('h5fc-ov-wifi-value', wifiTemps.length ? wifiTemps.map(function(s) { return s.temp_c + '\u00b0C'; }).join(' / ') : '\u2014');
		this.setText('h5fc-ov-wifi-hint', [sensors.wifi1, sensors.wifi2].filter(Boolean).map(function(s) { return s.label; }).filter(Boolean).join(' \u00b7 '));
		tempCard('h5fc-ov-modem', sensors.module);

		/* safety chips */
		this.setText('h5fc-ch-health-v', ctl.health === 'ok' ? _('Normal') : ctl.health === 'warn' ? _('Delayed') : ctl.health === 'fail' ? _('Fault') : _('Unknown'));
		this.setText('h5fc-ch-floor-v', safety.thermal_floor > 0 ? fmtPwm(safety.thermal_floor) : _('inactive'));
		this.setText('h5fc-ch-delay-v', safety.down_delay_active ? _('holding') : _('not waiting'));
		this.setText('h5fc-ch-owner-v', this.ownerName(safety.thermal_owner));
		this.setText('h5fc-ch-rpm-v', fan.rpm_supported ? (fan.rpm !== null && fan.rpm !== undefined ? fan.rpm + ' RPM' : '\u2014') : _('unsupported'));

		var note = document.getElementById('h5fc-ov-note');
		if (note) {
			note.className = 'h5fc-note';
			if (safety.cpu_throttling_intact === false) {
				note.className += ' fail';
				note.textContent = _('Warning: the CPU thermal governor reports user_space. Kernel CPU throttling may be disabled - check your firmware and the controller release notes.');
			}
			else if (ctl.result === 'failsafe') {
				note.className += ' warn';
				note.textContent = _('Failsafe cooling is active (%s). The controller detected a problem and drives a high output until it recovers.').format(this.reasonName(ctl.reason));
			}
			else if (safety.thermal_owner === 'userspace' || safety.thermal_owner === 'userspace-pwm') {
				note.textContent = _('The fan manager owns the fan policy. Kernel CPU throttling and hot/critical over-temperature protection remain active.');
			}
			else {
				note.textContent = _('Kernel thermal protection is always retained; when the kernel requires more cooling the effective output is raised automatically.');
			}
		}

		/* control tab live strip */
		var liveState = effectiveEnabled ? this.modeName(effectiveMode) : _('disabled');
		this.setText('h5fc-ctl-live', liveState);
		this.setText('h5fc-ctl-requested-value', fan.requested_pwm !== null && fan.requested_pwm !== undefined ? fmtPwm(fan.requested_pwm) : '\u2014');
		this.setText('h5fc-ctl-target-value', fan.target_pwm !== null && fan.target_pwm !== undefined ? fmtPwm(fan.target_pwm) : '\u2014');
		this.setText('h5fc-ctl-applied-value', fan.applied_pwm !== null && fan.applied_pwm !== undefined ? fmtPwm(fan.applied_pwm) : (fan.pwm !== undefined ? fmtPwm(fan.pwm) : '\u2014'));
		this.setText('h5fc-ctl-reason-value', this.reasonName(ctl.reason));
		this.setText('h5fc-ctl-reason-hint', ctl.last_error || '');

		/* curve tab */
		this.setText('h5fc-curve-now-value', (controlTemp !== null && ctl.mode === 'auto') ? fmtTemp(controlTemp) : '\u2014');
		this.setText('h5fc-curve-now-hint', (controlTemp !== null && ctl.mode === 'auto') ? _('target %s').format(fan.target_pwm !== undefined ? fmtPwm(fan.target_pwm) : '\u2014') : '');
		this.setText('h5fc-curve-floor-value', safety.thermal_floor > 0 ? fmtPwm(safety.thermal_floor) : '\u2014');
		this.setText('h5fc-curve-floor-hint', _('cooling state %s / %s').format(safety.thermal_state, safety.thermal_max_state));
		var curveNote = document.getElementById('h5fc-curve-note');
		if (curveNote) {
			curveNote.className = 'h5fc-note';
			curveNote.textContent = (effectiveCurve === 'custom')
				? _('Editing the custom curve. Invalid combinations are rejected before saving; the interpolation matches the controller exactly.')
				: _('Showing the %s preset from the saved configuration. Switch to Custom to edit your own points.').format(this.profileName(effectiveCurve));
		}
		this.drawCurveChart();

		/* monitor gauges */
		this.updateGauge('cpu', sensors.cpu, BANDS_CPU, sensors.control);
		this.updateGauge('phy', sensors.phy, BANDS_GENERIC, sensors.control);
		this.updateGauge('wifi1', sensors.wifi1, BANDS_GENERIC, sensors.control);
		this.updateGauge('wifi2', sensors.wifi2, BANDS_GENERIC, sensors.control);
		this.updateGauge('module', sensors.module, BANDS_GENERIC, sensors.control);
		this.setText('h5fc-mon-control', controlTemp !== null ? _('Control input: %s (%s)').format(fmtTemp(controlTemp), sensors.control.label || '-') : _('No valid control sensor'));

		/* diagnostics */
		var diagBadge = document.getElementById('h5fc-diag-health');
		if (diagBadge) {
			diagBadge.className = 'h5fc-badge ' + (ctl.health === 'ok' ? 'ok' : ctl.health === 'warn' ? 'warn' : 'fail');
			diagBadge.textContent = ctl.health === 'ok' ? _('Healthy') : ctl.health === 'warn' ? _('Stale status') : ctl.health === 'fail' ? _('Fault') : _('No data');
		}
		this.fillKv('h5fc-diag-ctl', [
			[_('Daemon running'), ctl.running ? _('yes') : _('no')],
			[_('Result'), ctl.result || '\u2014'],
			[_('Reason'), ctl.reason || '\u2014'],
			[_('State age'), ctl.state_age !== null && ctl.state_age !== undefined ? _('%d s').format(ctl.state_age) : '\u2014'],
			[_('Last success'), ctl.last_success ? new Date(ctl.last_success * 1000).toLocaleString() : '\u2014'],
			[_('Last error'), ctl.last_error || _('none')],
			[_('Controller version'), ctl.version || '\u2014'],
			[_('Status source'), st.source === 'snapshot' ? _('daemon snapshot') : _('live query')]
		]);
		this.fillKv('h5fc-diag-safety', [
			[_('Fan ownership'), this.ownerName(safety.thermal_owner)],
			[_('Kernel governor'), safety.kernel_policy || '\u2014'],
			[_('Thermal zone'), safety.kernel_zone || '\u2014'],
			[_('CPU throttling intact'), safety.cpu_throttling_intact === false ? _('NO - user_space detected!') : _('yes')],
			[_('Cooling state'), '%s / %s'.format(safety.thermal_state, safety.thermal_max_state)],
			[_('Kernel safety floor'), safety.thermal_floor > 0 ? fmtPwm(safety.thermal_floor) : _('inactive')],
			[_('Speed-down delay'), safety.down_delay_active ? _('holding output') : _('inactive')]
		]);
		var cfgErrors = this.validateAll();
		this.setHtml('h5fc-diag-cfg', cfgErrors.length
			? '<span style="color:var(--h5fc-red,#ef6262)">' + _('Problems found:') + ' ' + cfgErrors.join('; ') + '</span>'
			: _('All configured values are valid.'));
		var raw = document.getElementById('h5fc-diag-raw');
		if (raw) {
			try {
				raw.textContent = JSON.stringify(st, null, 2);
			}
			catch (e) {
				raw.textContent = String(st);
			}
		}
	},

	updateFanWidget: function(fan, ctl, safety) {
		var box = document.getElementById('h5fc-fanbox');
		var svg = document.getElementById('h5fc-fan-svg');
		if (!box || !svg)
			return;
		var pct = null;
		var applied = (fan.applied_pwm !== null && fan.applied_pwm !== undefined) ? fan.applied_pwm : fan.pwm;
		if (applied !== null && applied !== undefined)
			pct = Math.round(applied * 100 / 255);

		var running, color, label;
		if (ctl.health === 'fail') {
			running = true; /* keep spinning - fail-safe drives high output */
			color = 'var(--h5fc-amber,#f0aa46)';
			label = _('FAILSAFE');
		}
		else if (applied === null || applied === undefined) {
			running = false;
			color = 'var(--text-color-low,#999)';
			label = _('UNKNOWN');
		}
		else if (applied === 0) {
			running = false;
			color = 'var(--text-color-medium,#888)';
			label = _('STOPPED');
		}
		else {
			running = true;
			color = 'var(--h5fc-ok,#36c98f)';
			label = pct + '%';
		}

		box.className = 'h5fc-card h5fc-fanbox' + (running ? ' h5fc-fan-run' : '');
		svg.style.color = color;
		var blades = document.getElementById('h5fc-fan-blades');
		if (blades && running && pct !== null) {
			/* purely illustrative pace, not an RPM reading */
			blades.style.animationDuration = (4.2 - 3.2 * clamp(pct, 5, 100) / 100).toFixed(2) + 's';
		}
		this.setText('h5fc-fan-label', label);
		svg.setAttribute('aria-label', _('Fan illustration: %s, driven at %s').format(
			label === '\u2014' ? _('unknown') : label,
			applied === null || applied === undefined ? _('unknown output') : fmtPwm(applied)
		));
	},

	updateGauge: function(id, sensor, bands, control) {
		var box = document.getElementById('h5fc-gauge-' + id);
		if (!box)
			return;
		var temp = sensor ? sensor.temp_c : null;
		var band = bandOf(bands, temp);
		var svg = document.getElementById('h5fc-gauge-' + id + '-svg');
		if (svg) {
			/* scale: -10..110 -> 0..100% */
			var pct = (temp === null) ? 0 : clamp((temp + 10) * 100 / 120, 0, 100);
			var angle = Math.PI * (1 - pct / 100);
			var ex = 100 + 78 * Math.cos(angle), ey = 96 - 78 * Math.sin(angle);
			var arc = document.getElementById('h5fc-gauge-' + id + '-svg-arc');
			if (arc) {
				arc.setAttribute('d', 'M22 96 A78 78 0 0 1 ' + ex.toFixed(1) + ' ' + ey.toFixed(1));
				var color = band ? { ok: 'var(--h5fc-ok,#36c98f)', warn: 'var(--h5fc-amber,#f0aa46)', hot: 'var(--h5fc-red,#ef6262)', crit: 'var(--h5fc-red,#ef6262)' }[band.cls] : 'rgba(127,127,127,.3)';
				arc.setAttribute('stroke', color);
				arc.setAttribute('opacity', temp === null ? '.25' : '.95');
			}
			this.setText('h5fc-gauge-' + id + '-svg-val', temp === null ? '\u2014' : String(temp));
			svg.setAttribute('aria-label', temp === null ? _('Temperature unknown') : _('%d degrees Celsius, %s').format(temp, band ? band.label : ''));
		}
		var bandEl = document.getElementById('h5fc-gauge-' + id + '-band');
		if (bandEl) {
			bandEl.className = 'h5fc-badge ' + (band ? band.cls === 'ok' ? 'ok' : band.cls === 'warn' ? 'warn' : 'fail' : 'mute');
			bandEl.style.fontSize = '10px';
			bandEl.textContent = band ? band.label : _('No data');
		}
		this.setText('h5fc-gauge-' + id + '-src', sensor ? this.sourceName(sensor.source) + (sensor.label ? ' \u00b7 ' + sensor.label : '') : '\u2014');
		var isControl = control && control.label && sensor && sensor.label === control.label;
		box.classList.toggle('active', !!isControl);
	},

	/* ----------------------------------------------------------- *
	 * widget state refresh (after load/reset)                     *
	 * ----------------------------------------------------------- */

	rebuildStaticWidgets: function() {
		var el;
		el = document.getElementById('h5fc-enabled');
		if (el)
			el.checked = this.values.enabled === '1';
		this.checkSegmented('h5fc-mode', this.values.mode);
		this.checkSegmented('h5fc-source', this.values.temp_source);
		this.checkSegmented('h5fc-curve-profile', this.values.curve);
		this.syncManualSlider();
		this.updateModeVisibility();
		['interval', 'hysteresis', 'down_delay', 'start_pwm', 'start_boost_ms', 'fail_pwm'].forEach(function(key) {
			var input = document.getElementById('h5fc-set-' + key);
			if (input) {
				input.value = this.values[key];
				input.classList.remove('err');
			}
		}, this);
		this.rebuildCurveEditor();
		this.drawCurveChart();
	},

	checkSegmented: function(id, value) {
		var inputs = document.querySelectorAll('#' + id + ' input[type=radio]');
		for (var i = 0; i < inputs.length; i++)
			inputs[i].checked = inputs[i].value === value;
	},

	syncManualSlider: function() {
		var range = document.getElementById('h5fc-manual-range');
		var number = document.getElementById('h5fc-manual-number');
		var v = clamp(parseInt(this.values.manual_pwm, 10) || 0, 0, 255);
		if (range)
			range.value = v;
		if (number)
			number.value = v;
	},

	wireManualSlider: function() {
		/* One-time two-way binding between slider and number input. */
		var self = this;
		var range = document.getElementById('h5fc-manual-range');
		var number = document.getElementById('h5fc-manual-number');
		if (!range || !number || range.dataset.h5fcWired)
			return;
		range.dataset.h5fcWired = '1';
		var sync = function(value, source) {
			var v = clamp(parseInt(value, 10) || 0, 0, 255);
			if (source !== range)
				range.value = v;
			if (source !== number)
				number.value = v;
			self.setValue('manual_pwm', String(v));
		};
		range.addEventListener('input', function(ev) { sync(ev.target.value, range); });
		number.addEventListener('input', function(ev) {
			var valid = validateOption('manual_pwm', ev.target.value) === true;
			number.classList.toggle('err', !valid);
			if (valid)
				sync(ev.target.value, number);
		});
	},

	updateModeVisibility: function() {
		var manualRow = document.getElementById('h5fc-manual-row');
		if (manualRow)
			manualRow.style.opacity = (this.values.mode === 'manual' && this.values.enabled === '1') ? '1' : '.5';
		var editor = document.getElementById('h5fc-curve-editor');
		if (editor)
			editor.style.opacity = (this.values.curve === 'custom') ? '1' : '.45';
	}
});
