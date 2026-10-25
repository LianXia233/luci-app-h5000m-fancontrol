'use strict';
'require view';
'require uci';
'require fs';
'require poll';
'require ui';

/*
 * H5000M cooling management.
 *
 * Live numbers come only from `/usr/sbin/h5000m-fancontrol-status json`, the
 * machine readable contract documented in docs/status-api.md. Configuration is
 * edited through LuCI's uci API, so Save & Apply commits one transaction and
 * the running controller picks it up without a service restart.
 *
 * Nothing here estimates a value the hardware cannot produce: an unread
 * temperature stays "unknown", and RPM is rendered only when the board exposes
 * a tachometer. The fan animation reflects the applied output, never a
 * measured shaft speed.
 */

var STATUS_CMD = '/usr/sbin/h5000m-fancontrol-status';
var APPLY_CMD = '/usr/sbin/h5000m-fancontrol';
var INITD_CMD = '/etc/init.d/h5000m-fancontrol';
var CFG = 'h5000m_fancontrol';
var SID = 'settings';
var TAB_KEY = 'luci.h5000m-fancontrol.tab';
var REFRESH_SECONDS = 5;
var SVG_NS = 'http://www.w3.org/2000/svg';

/*
 * Reference bands are this project's own assessment, aligned with the knee
 * points of the shipped curves. They are not vendor datasheet limits, and the
 * UI says so instead of implying a manufacturer specification.
 */
var BANDS = {
	cpu: { watch: 70, hot: 85, critical: 95 },
	phy: { watch: 70, hot: 80, critical: 90 },
	radio: { watch: 75, hot: 88, critical: 95 },
	modem: { watch: 70, hot: 82, critical: 90 }
};

var SENSOR_SLOTS = [
	{ id: 'cpu', title: 'CPU', band: BANDS.cpu },
	{ id: 'phy', title: 'Ethernet PHY', band: BANDS.phy },
	{ id: 'wifi1', title: 'Wi-Fi radio 1', band: BANDS.radio },
	{ id: 'wifi2', title: 'Wi-Fi radio 2', band: BANDS.radio },
	{ id: 'module', title: '5G modem', band: BANDS.modem }
];

var TABS = [
	{ id: 'overview', label: 'Overview' },
	{ id: 'control', label: 'Fan control' },
	{ id: 'curve', label: 'Fan curve' },
	{ id: 'monitor', label: 'Temperature' },
	{ id: 'system', label: 'Settings' },
	{ id: 'diagnostics', label: 'Diagnostics' }
];

/*
 * Every editable option in one place: widget kind, bounds, shipped default and
 * the tab that owns it. `policy: true` marks the options whose value changes
 * need a control cycle, which keeps Save from poking the controller when only
 * cosmetic values moved.
 */
var OPTIONS = {
	enabled: {
		kind: 'switch', tab: 'control', policy: true, def: '1',
		label: 'Enable enhanced controller',
		hint: 'When disabled the fan follows the kernel thermal policy only; CPU throttling and over-temperature protection are unaffected.',
		choices: [['1', 'Enabled'], ['0', 'Disabled']]
	},
	mode: {
		kind: 'select', tab: 'control', policy: true, def: 'auto',
		label: 'Operating mode',
		hint: 'Automatic uses the fan curve, Manual keeps a fixed output, Kernel protection only leaves the fan to the kernel.',
		choices: [['auto', 'Automatic'], ['manual', 'Manual'], ['kernel', 'Kernel protection only']]
	},
	curve: {
		kind: 'select', tab: 'control', policy: true, def: 'balanced',
		label: 'Cooling profile',
		hint: 'Only used in automatic mode.',
		choices: [['silent', 'Quiet'], ['balanced', 'Balanced'], ['performance', 'Performance'], ['custom', 'Custom']]
	},
	manual_pwm: {
		kind: 'slider', tab: 'control', policy: true, def: '160', min: 0, max: 255, unit: '0-255',
		label: 'Manual PWM output',
		hint: 'The kernel safety floor can still raise the effective output above this value.'
	},
	start_pwm: {
		kind: 'number', tab: 'control', policy: true, def: '192', min: 80, max: 255, unit: '0-255',
		label: 'Startup boost PWM',
		hint: 'Short high pulse written only when the fan is standing still.'
	},
	start_boost_ms: {
		kind: 'number', tab: 'control', policy: true, def: '700', min: 0, max: 3000, unit: 'ms',
		label: 'Startup boost duration',
		hint: 'Set 0 to disable the boost.'
	},
	temp_source: {
		kind: 'select', tab: 'system', policy: true, def: 'max',
		label: 'Control temperature',
		hint: 'The hottest option considers CPU, Ethernet PHY, both radios and the 5G modem cache.',
		choices: [['max', 'Hottest valid sensor'], ['cpu', 'CPU only']]
	},
	interval: {
		kind: 'number', tab: 'system', policy: true, def: '5', min: 2, max: 60, unit: 's',
		label: 'Sampling interval',
		hint: 'Seconds between control decisions. The page refreshes independently of this.'
	},
	hysteresis: {
		kind: 'number', tab: 'system', policy: true, def: '2', min: 0, max: 10, unit: '°C',
		label: 'Temperature hysteresis',
		hint: 'Extra degrees applied when slowing down, so the fan does not oscillate around a curve point.'
	},
	down_delay: {
		kind: 'number', tab: 'system', policy: true, def: '30', min: 0, max: 300, unit: 's',
		label: 'Spin-down delay',
		hint: 'Hold the current output this long before slowing down. Speed-ups stay immediate.'
	},
	fail_pwm: {
		kind: 'number', tab: 'system', policy: true, def: '255', min: 192, max: 255, unit: '0-255',
		label: 'Failsafe output',
		hint: 'Used when no sensor or no usable curve is available; the range keeps it above 75%.'
	},
	curve_custom: {
		kind: 'curve', tab: 'curve', policy: true, def: '20:0,40:50,55:60,70:75,85:90,95:100',
		label: 'Custom curve',
		hint: 'Comma separated temperature:percent pairs, temperatures strictly increasing.'
	},
	curve_silent: {
		kind: 'curve', tab: 'system', policy: true, def: '20:0,35:0,40:50,55:50,70:60,85:75,95:100',
		label: 'Quiet curve',
		hint: 'Preset used by the Quiet profile. Restore defaults puts this line back.'
	},
	curve_balanced: {
		kind: 'curve', tab: 'system', policy: true, def: '20:0,35:0,40:50,55:60,70:75,85:90,95:100',
		label: 'Balanced curve',
		hint: 'Preset used by the Balanced profile.'
	},
	curve_performance: {
		kind: 'curve', tab: 'system', policy: true, def: '20:0,30:60,45:70,60:82,75:92,85:100',
		label: 'Performance curve',
		hint: 'Preset used by the Performance profile.'
	}
};

var POLICY_KEYS = Object.keys(OPTIONS).filter(function(key) {
	return OPTIONS[key].policy;
});

/* Which stored curve each profile uses, mirroring config::curve_spec(). */
var CURVE_OPTION = {
	silent: 'curve_silent',
	balanced: 'curve_balanced',
	performance: 'curve_performance',
	custom: 'curve_custom'
};

/* ---------------------------------------------------------------- helpers */

function num(value) {
	var parsed = parseInt(value, 10);
	return isNaN(parsed) ? null : parsed;
}

function isUInt(value) {
	return /^[0-9]+$/.test(String(value === null || value === undefined ? '' : value).trim());
}

function known(value) {
	return value !== null && value !== undefined && value !== '';
}

function clamp(value, min, max) {
	return Math.max(min, Math.min(max, value));
}

/* Matches the backend's percent_of_pwm rounding, so both agree on one number. */
function percentOfPwm(value) {
	return Math.round(clamp(value, 0, 255) * 100 / 255);
}

function dash(value) {
	return known(value) ? String(value) : '\u2014';
}

function formatTemp(value) {
	return known(value) ? _('%s °C').format(value) : _('Unknown');
}

function formatAge(seconds) {
	if (!known(seconds))
		return _('Never sampled');

	if (seconds <= 1)
		return _('Just now');

	return _('Last %s s ago').format(seconds);
}

function formatClock(seconds) {
	var date = new Date(seconds * 1000);

	if (isNaN(date.getTime()))
		return '\u2014';

	return '%04d-%02d-%02d %02d:%02d:%02d'.format(
		date.getFullYear(), date.getMonth() + 1, date.getDate(),
		date.getHours(), date.getMinutes(), date.getSeconds());
}

/*
 * Curve parsing and evaluation, mirroring the Rust backend exactly:
 * non-negative integers only, temperature strictly increasing and within 150,
 * percent within 100, linear interpolation with integer division truncating
 * towards zero, then percent * 255 / 100.
 */
function parseCurve(spec) {
	var points = [], tokens = String(spec || '').split(','), previous = null, i;

	for (i = 0; i < tokens.length; i++) {
		var token = tokens[i].trim();

		if (token === '')
			continue;

		var parts = token.split(':');

		if (parts.length !== 2)
			return { points: [], error: _('Write each point as temperature:percent, for example 40:50.') };

		if (!isUInt(parts[0]) || !isUInt(parts[1]))
			return { points: [], error: _('%s is not a pair of whole numbers.').format(token) };

		var temp = parseInt(parts[0], 10), percent = parseInt(parts[1], 10);

		if (temp > 150)
			return { points: [], error: _('Temperature %s °C is above the supported 150 °C.').format(temp) };

		if (percent > 100)
			return { points: [], error: _('Percentage %s is above 100%.').format(percent) };

		if (previous !== null && temp <= previous)
			return { points: [], error: _('Temperature %s °C does not rise above %s °C.').format(temp, previous) };

		points.push({ temp: temp, percent: percent });
		previous = temp;
	}

	if (points.length < 2)
		return { points: points, error: _('The curve needs at least two points.') };

	if (points.length > 32)
		return { points: points, error: _('The curve accepts at most 32 points.') };

	return { points: points, error: null };
}

function curvePercentAt(points, temp) {
	var i;

	if (!points || points.length < 2)
		return null;

	if (temp <= points[0].temp)
		return points[0].percent;

	for (i = 1; i < points.length; i++) {
		if (temp <= points[i].temp) {
			var previous = points[i - 1], span = points[i].temp - previous.temp;

			if (span === 0)
				return points[i].percent;

			return previous.percent + Math.trunc((temp - previous.temp) * (points[i].percent - previous.percent) / span);
		}
	}

	return points[points.length - 1].percent;
}

function curveRawAt(points, temp) {
	var percent = curvePercentAt(points, temp);
	return percent === null ? null : Math.trunc(percent * 255 / 100);
}

function svgEl(name, attrs, children) {
	var node = document.createElementNS(SVG_NS, name), key;

	for (key in (attrs || {})) {
		if (!attrs.hasOwnProperty(key) || attrs[key] === null || attrs[key] === undefined)
			continue;

		node.setAttribute(key, String(attrs[key]));
	}

	(children || []).forEach(function(child) {
		if (child === null || child === undefined)
			return;

		node.appendChild(typeof child === 'string' ? document.createTextNode(child) : child);
	});

	return node;
}

function show(node, visible) {
	node.setAttribute('display', visible ? 'inline' : 'none');
}

function clear(node) {
	while (node.firstChild)
		node.removeChild(node.firstChild);
}

function row(labelText, valueNode) {
	return E('div', { 'class': 'h5fan-row' }, [
		E('span', { 'class': 'h5fan-row-key' }, [ _(labelText) ]),
		valueNode
	]);
}

function textValue(text, tone) {
	return E('span', { 'class': 'h5fan-row-val%s'.format(known(tone) ? ' is-%s'.format(tone) : '') }, [ String(text) ]);
}

function tag(text, tone) {
	return E('span', { 'class': 'h5fan-tag%s'.format(known(tone) ? ' is-%s'.format(tone) : '') }, [ String(text) ]);
}

/* ------------------------------------------------------------------ views */

function mountFanDiagram() {
	/*
	 * Geometry of the turbine design: everything is anchored on the rotor
	 * centre, the 270° progress ring spans 706.86 of the r=150 circumference
	 * and starts at 135°, so the anchor dot sits at 135° + 2.7° per percent.
	 */
	var CX = 210, CY = 195, RING_R = 150, RING_CIRC = 942.48, RING_ARC = 706.86;
	var blades = [], i;

	for (i = 0; i < 8; i++) {
		blades.push(svgEl('use', { href: '#h5fan-blade', transform: 'rotate(%d)'.format(i * 45) }));
	}

	var defs = svgEl('defs', {}, [
		svgEl('linearGradient', { id: 'h5fan-plate-grad', x1: '0%', y1: '0%', x2: '100%', y2: '100%' }, [
			svgEl('stop', { offset: '0%', 'stop-color': '#ffffff' }),
			svgEl('stop', { offset: '100%', 'stop-color': '#f1f5f9' })
		]),
		svgEl('radialGradient', { id: 'h5fan-tunnel-grad', cx: '50%', cy: '50%', r: '50%' }, [
			svgEl('stop', { offset: '0%', 'stop-color': '#e0f2fe', 'stop-opacity': '0.6' }),
			svgEl('stop', { offset: '65%', 'stop-color': '#f8fafc', 'stop-opacity': '0.9' }),
			svgEl('stop', { offset: '100%', 'stop-color': '#e2e8f0', 'stop-opacity': '1' })
		]),
		svgEl('linearGradient', { id: 'h5fan-progress-grad', x1: '0%', y1: '100%', x2: '100%', y2: '0%' }, [
			svgEl('stop', { offset: '0%', 'stop-color': '#38bdf8' }),
			svgEl('stop', { offset: '50%', 'stop-color': '#0284c7' }),
			svgEl('stop', { offset: '100%', 'stop-color': '#2563eb' })
		]),
		svgEl('linearGradient', { id: 'h5fan-blade-grad', x1: '0%', y1: '100%', x2: '100%', y2: '0%' }, [
			svgEl('stop', { offset: '0%', 'stop-color': '#0284c7', 'stop-opacity': '0.15' }),
			svgEl('stop', { offset: '55%', 'stop-color': '#38bdf8', 'stop-opacity': '0.55' }),
			svgEl('stop', { offset: '100%', 'stop-color': '#0ea5e9', 'stop-opacity': '0.85' })
		]),
		svgEl('linearGradient', { id: 'h5fan-blade-edge', x1: '0%', y1: '0%', x2: '0%', y2: '100%' }, [
			svgEl('stop', { offset: '0%', 'stop-color': '#bae6fd', 'stop-opacity': '0.9' }),
			svgEl('stop', { offset: '100%', 'stop-color': '#0284c7', 'stop-opacity': '0.2' })
		]),
		svgEl('linearGradient', { id: 'h5fan-hub-grad', x1: '0%', y1: '0%', x2: '100%', y2: '100%' }, [
			svgEl('stop', { offset: '0%', 'stop-color': '#ffffff' }),
			svgEl('stop', { offset: '100%', 'stop-color': '#cbd5e1' })
		]),
		svgEl('g', { id: 'h5fan-blade' }, [
			svgEl('path', {
				d: 'M -6,-24 C -36,-54 -26,-102 18,-114 C 30,-117 36,-108 30,-96 C 12,-66 10,-42 20,-22 Z',
				fill: 'url(#h5fan-blade-grad)', stroke: 'url(#h5fan-blade-edge)', 'stroke-width': '1.5'
			}),
			svgEl('path', {
				d: 'M 2,-26 C -16,-54 -8,-90 22,-106', fill: 'none',
				stroke: '#e0f2fe', 'stroke-width': '1.2', 'stroke-linecap': 'round', opacity: '0.7'
			})
		])
	]);

	var rotor = svgEl('g', { 'class': 'h5fan-rotor' }, [
		svgEl('g', { transform: 'translate(%d, %d)'.format(CX, CY) }, blades)
	]);
	var airflow = svgEl('g', { 'class': 'h5fan-airflow' }, [
		svgEl('circle', {
			cx: CX, cy: CY, r: 92, fill: 'none', stroke: '#38bdf8', 'stroke-width': '1.2',
			'stroke-dasharray': '18 28 8 22', 'stroke-linecap': 'round', opacity: '0.35'
		}),
		svgEl('circle', {
			cx: CX, cy: CY, r: 64, fill: 'none', stroke: '#0284c7', 'stroke-width': '1',
			'stroke-dasharray': '12 32', 'stroke-linecap': 'round', opacity: '0.25'
		})
	]);
	var progress = svgEl('circle', {
		cx: CX, cy: CY, r: RING_R, fill: 'none', 'class': 'h5fan-progress-value',
		'stroke-width': '8', 'stroke-linecap': 'round',
		'stroke-dasharray': '0 %s'.format(RING_CIRC),
		transform: 'rotate(135 %d %d)'.format(CX, CY)
	});
	var track = svgEl('circle', {
		cx: CX, cy: CY, r: RING_R, fill: 'none', stroke: '#e2e8f0', 'stroke-width': '8',
		'stroke-linecap': 'round', 'stroke-dasharray': '%s %s'.format(RING_ARC, RING_CIRC),
		transform: 'rotate(135 %d %d)'.format(CX, CY)
	});
	var anchor = svgEl('circle', {
		cx: CX, cy: CY - RING_R, r: 6, fill: '#ffffff', 'class': 'h5fan-progress-anchor',
		stroke: '#2563eb', 'stroke-width': '3', display: 'none'
	});
	var capsuleLabel = svgEl('text', {
		x: 178, y: 347.5, 'class': 'h5fan-capsule-label'
	}, [ _('Loading…') ]);
	var capsuleValue = svgEl('text', {
		x: 216, y: 348, 'class': 'h5fan-capsule-value'
	}, [ '\u2014' ]);

	var svg = svgEl('svg', {
		viewBox: '0 0 420 420', 'class': 'h5fan-fan', role: 'img',
		preserveAspectRatio: 'xMidYMid meet', 'aria-label': _('Fan diagram')
	}, [
		svgEl('title', {}, [ _('Fan diagram') ]),
		svgEl('desc', {}, [ _('Schematic fan. The animation follows the applied controller output and is not a measured shaft speed.') ]),
		defs,
		svgEl('circle', { cx: CX, cy: CY, r: 168, fill: 'url(#h5fan-plate-grad)', stroke: '#ffffff', 'stroke-width': '2' }),
		track,
		progress,
		anchor,
		svgEl('circle', { cx: CX, cy: CY, r: 130, fill: 'url(#h5fan-tunnel-grad)', stroke: '#cbd5e1', 'stroke-width': '1', 'stroke-opacity': '0.6' }),
		svgEl('circle', { cx: CX, cy: CY, r: 122, fill: 'none', stroke: '#94a3b8', 'stroke-width': '3', 'stroke-dasharray': '1.5 11.27', opacity: '0.45' }),
		airflow,
		rotor,
		svgEl('g', {}, [
			svgEl('circle', { cx: CX, cy: CY, r: 34, fill: 'url(#h5fan-hub-grad)', stroke: '#ffffff', 'stroke-width': '2' }),
			svgEl('circle', { cx: CX, cy: CY, r: 24, fill: '#f8fafc', stroke: '#e2e8f0', 'stroke-width': '1.5' }),
			svgEl('circle', { cx: CX, cy: CY, r: 9, fill: 'url(#h5fan-progress-grad)' }),
			svgEl('circle', { cx: CX - 2.5, cy: CY - 2.5, r: 2.5, fill: '#ffffff', opacity: '0.8' })
		]),
		svgEl('g', {}, [
			svgEl('rect', { x: 142, y: 322, width: 136, height: 40, rx: 20, fill: '#ffffff', stroke: '#f1f5f9', 'stroke-width': '1.5' }),
			svgEl('circle', { cx: 164, cy: 342, r: 4.5, fill: '#0ea5e9', 'class': 'h5fan-dot' }),
			svgEl('circle', { cx: 164, cy: 342, r: 8, fill: '#0ea5e9', opacity: '0.15' }),
			capsuleLabel,
			capsuleValue
		])
	]);

	svg.h5update = function(status) {
		var fan = status.fan || {}, controller = status.controller || {};
		var health = controller.health || 'stopped';
		var applied = known(fan.applied_pwm) ? fan.applied_pwm : null;
		var live = known(fan.pwm_value) ? fan.pwm_value : applied;
		var percent = known(live) ? percentOfPwm(live) : null;
		var classes = [ 'h5fan-fan' ];

		if (health === 'error' || health === 'stopped')
			classes.push('is-unknown');
		else if (health === 'failsafe')
			classes.push('is-fault');
		else if (percent === null || percent === 0)
			classes.push('is-stopped');
		else if (percent < 34)
			classes.push('is-slow');
		else if (percent < 67)
			classes.push('is-medium');
		else
			classes.push('is-fast');

		svg.setAttribute('class', classes.join(' '));

		if (percent === null) {
			progress.setAttribute('stroke-dasharray', '0 %s'.format(RING_CIRC));
			anchor.setAttribute('display', 'none');
			capsuleValue.textContent = '\u2014';
			rotor.setAttribute('style', '');
		} else {
			progress.setAttribute('stroke-dasharray', '%s %s'.format(
				(RING_ARC * percent / 100).toFixed(2), RING_CIRC));
			var angle = (135 + 2.7 * percent) * Math.PI / 180;
			anchor.setAttribute('cx', (CX + RING_R * Math.cos(angle)).toFixed(2));
			anchor.setAttribute('cy', (CY + RING_R * Math.sin(angle)).toFixed(2));
			anchor.setAttribute('display', 'inline');
			capsuleValue.textContent = '%d%%'.format(percent);
			/* The rotor spins faster the harder the fan is driven. */
			rotor.setAttribute('style', 'animation-duration:%s'.format(
				(4.6 - 3.7 * percent / 100).toFixed(2) + 's'));
		}

		if (health === 'failsafe')
			capsuleLabel.textContent = _('Failsafe');
		else if (health === 'error')
			capsuleLabel.textContent = _('Error');
		else if (health === 'stopped')
			capsuleLabel.textContent = _('Stopped');
		else if (percent === 0)
			capsuleLabel.textContent = _('Idle');
		else
			capsuleLabel.textContent = _('Output');

		svg.setAttribute('aria-label', _('Fan output %1$s, %2$s').format(
			percent === null ? _('unknown') : '%d%%'.format(percent), capsuleLabel.textContent));
	};

	return svg;
}

function mountGauge(label) {
	var radius = 44, circumference = 2 * Math.PI * radius;
	var value = svgEl('circle', {
		cx: 58, cy: 58, r: radius, 'class': 'h5fan-gauge-value',
		'stroke-dasharray': '0 %s'.format(circumference),
		transform: 'rotate(-90 58 58)'
	});
	var number = svgEl('text', { x: 58, y: 60, 'class': 'h5fan-gauge-num', 'text-anchor': 'middle' }, [ '\u2014' ]);
	var svg = svgEl('svg', {
		viewBox: '0 0 116 116', 'class': 'h5fan-gauge', role: 'img', 'aria-label': _(label)
	}, [
		svgEl('title', {}, [ _(label) ]),
		value,
		svgEl('circle', { cx: 58, cy: 58, r: radius, 'class': 'h5fan-gauge-track' }),
		number,
		svgEl('text', { x: 58, y: 78, 'class': 'h5fan-gauge-unit', 'text-anchor': 'middle' }, [ '% ' + _('of full speed') ])
	]);

	svg.h5update = function(percent, tone) {
		var shown = known(percent) ? clamp(percent, 0, 100) : 0;

		value.setAttribute('stroke-dasharray', '%s %s'.format(circumference * shown / 100, circumference));
		value.setAttribute('class', 'h5fan-gauge-value is-%s'.format(tone || 'idle'));
		number.textContent = known(percent) ? '%d'.format(shown) : '\u2014';
		svg.setAttribute('aria-label', '%s: %s'.format(_(label),
			known(percent) ? '%d%%'.format(shown) : _('Unavailable')));
	};

	return svg;
}

function mountTempBar(slot) {
	var width = 240, height = 62, min = 20, max = 105, inner = width - 24;
	var title = svgEl('text', { x: 12, y: 15, 'class': 'h5fan-bar-name' }, [ _(slot.title) ]);
	var value = svgEl('text', { x: width - 12, y: 15, 'class': 'h5fan-bar-value', 'text-anchor': 'end' }, [ _('Unknown') ]);
	var track = svgEl('rect', { x: 12, y: 28, width: inner, height: 11, rx: 5.5, 'class': 'h5fan-bar-track' });
	var fill = svgEl('rect', { x: 12, y: 28, width: 0, height: 11, rx: 5.5, 'class': 'h5fan-bar-fill' });
	var hatched = svgEl('rect', { x: 12, y: 28, width: inner, height: 11, rx: 5.5, 'class': 'h5fan-bar-hatched' });
	var note = svgEl('text', { x: 12, y: 55, 'class': 'h5fan-bar-note' }, [ '' ]);
	var marks = [ slot.band.watch, slot.band.hot, slot.band.critical ].map(function(level) {
		var x = 12 + (clamp(level, min, max) - min) / (max - min) * inner;
		return svgEl('line', { x1: x, y1: 24, x2: x, y2: 43, 'class': 'h5fan-bar-tick' });
	});
	var svg = svgEl('svg', {
		viewBox: '0 0 %d %d'.format(width, height), 'class': 'h5fan-bar',
		role: 'img', 'aria-label': _(slot.title)
	}, [ svgEl('title', {}, [ _(slot.title) ]), track, hatched, fill, title, value, note ].concat(marks));

	svg.h5update = function(reading) {
		var has = !!(reading && reading.valid && known(reading.value_c));
		var tone = 'unknown';

		show(hatched, !has);
		fill.setAttribute('width', has ? String(inner * (clamp(reading.value_c, min, max) - min) / (max - min)) : '0');

		if (has) {
			if (reading.value_c >= slot.band.critical)
				tone = 'critical';
			else if (reading.value_c >= slot.band.hot)
				tone = 'hot';
			else if (reading.value_c >= slot.band.watch)
				tone = 'watch';
			else
				tone = 'ok';
		} else if (reading && reading.status === 'stale') {
			tone = 'stale';
		} else if (reading && (reading.status === 'invalid' || reading.status === 'failed')) {
			tone = 'bad';
		}

		fill.setAttribute('class', 'h5fan-bar-fill is-%s'.format(tone));
		value.textContent = has ? formatTemp(reading.value_c) : _('Unknown');
		note.textContent = reading
			? (has
				? '%s · %s'.format(reading.source || '\u2014', formatAge(reading.age_s))
				: '%s · %s'.format(_(stateName(reading.status)), reading.error || _('no reading')))
			: '';
		svg.setAttribute('aria-label', '%s: %s'.format(_(slot.title),
			has ? formatTemp(reading.value_c) : _('Unavailable')));
	};

	return svg;
}

function mountCurveChart() {
	/*
	 * Geometry of the curve card design: the plot spans 20°..110 °C across
	 * x=88..808 and 0..100% down y=130..390, everything else hangs off those
	 * anchors.
	 */
	var left = 88, right = 808, top = 130, bottom = 390, plotWidth = right - left;
	var minTemp = 20, maxTemp = 110;

	function xFor(temp) {
		return left + clamp((temp - minTemp) / (maxTemp - minTemp), 0, 1) * plotWidth;
	}

	function yFor(percent) {
		return bottom - clamp(percent, 0, 100) * (bottom - top) / 100;
	}

	var grid = svgEl('g', { 'class': 'h5fan-grid' });
	var axis = svgEl('g', { 'class': 'h5fan-axis' });
	var area = svgEl('path', { 'class': 'h5fan-area', fill: 'url(#h5fan-area-grad)' });
	var line = svgEl('path', {
		'class': 'h5fan-curve', fill: 'none',
		stroke: 'url(#h5fan-curve-grad)', 'stroke-width': '3.6',
		'stroke-linecap': 'round', 'stroke-linejoin': 'round', filter: 'url(#h5fan-glow)'
	});
	var floorLine = svgEl('line', {
		'class': 'h5fan-floor', x1: left, x2: right,
		stroke: '#ef4444', 'stroke-width': '2.2', 'stroke-dasharray': '8 6', opacity: '0.85'
	});
	var floorLabel = svgEl('text', {
		x: 766, y: 278.5, 'class': 'h5fan-line-pill-text is-floor', 'text-anchor': 'middle'
	}, [ '' ]);
	var floorPill = svgEl('rect', {
		x: 730, y: 266, width: 72, height: 18, rx: 4, 'class': 'h5fan-line-pill is-floor'
	});
	var targetLine = svgEl('line', {
		'class': 'h5fan-target', x1: left, x2: right,
		stroke: '#3b82f6', 'stroke-width': '2.2', 'stroke-dasharray': '4 5', opacity: '0.9'
	});
	var targetLabel = svgEl('text', {
		x: 766, y: 235.5, 'class': 'h5fan-line-pill-text is-target', 'text-anchor': 'middle'
	}, [ '' ]);
	var targetPill = svgEl('rect', {
		x: 730, y: 223, width: 72, height: 18, rx: 4, 'class': 'h5fan-line-pill is-target'
	});
	var cursor = svgEl('line', {
		'class': 'h5fan-cursor', y1: top, y2: bottom,
		stroke: 'url(#h5fan-beam-grad)', 'stroke-width': '2'
	});
	var markerPulse = svgEl('circle', { r: 10, 'class': 'h5fan-marker-pulse' });
	var marker = svgEl('circle', { r: 8.5, 'class': 'h5fan-marker' });
	var tooltipText = svgEl('text', {
		x: 0, y: 18.5, 'class': 'h5fan-tooltip-text', 'text-anchor': 'middle'
	}, [ '' ]);
	var tooltip = svgEl('g', { 'class': 'h5fan-tooltip', display: 'none' }, [
		svgEl('rect', { x: -52, y: 0, width: 104, height: 28, rx: 8, 'class': 'h5fan-tooltip-box' }),
		svgEl('path', { d: 'M -5,28 L 0,34 L 5,28 Z', 'class': 'h5fan-tooltip-box' }),
		tooltipText
	]);
	var nodes = svgEl('g', {});
	var error = svgEl('text', {
		x: (left + right) / 2, y: (top + bottom) / 2, 'class': 'h5fan-chart-error', 'text-anchor': 'middle'
	}, [ '' ]);

	[ 0, 25, 50, 75, 100 ].forEach(function(level) {
		grid.appendChild(svgEl('line', {
			x1: left, x2: right, y1: yFor(level), y2: yFor(level),
			'class': level % 50 ? 'h5fan-grid-line is-dashed' : 'h5fan-grid-line'
		}));
		axis.appendChild(svgEl('text', {
			x: left - 14, y: yFor(level) + 4.5, 'class': 'h5fan-tick', 'text-anchor': 'end'
		}, [ '%d%%'.format(level) ]));
	});

	[ 20, 35, 50, 65, 80, 95, 110 ].forEach(function(temp) {
		grid.appendChild(svgEl('line', {
			x1: xFor(temp), x2: xFor(temp), y1: top, y2: bottom,
			'class': temp === minTemp || temp === maxTemp ? 'h5fan-grid-line is-edge' : 'h5fan-grid-line is-faint'
		}));
		axis.appendChild(svgEl('text', {
			x: xFor(temp), y: bottom + 24, 'class': 'h5fan-tick', 'text-anchor': 'middle'
		}, [ '%d°'.format(temp) ]));
	});

	axis.appendChild(svgEl('line', {
		x1: left, x2: left, y1: top, y2: bottom, 'class': 'h5fan-axis-line'
	}));
	axis.appendChild(svgEl('line', {
		x1: left, x2: right, y1: bottom, y2: bottom, 'class': 'h5fan-axis-line is-baseline'
	}));
	axis.appendChild(svgEl('text', {
		x: left, y: bottom + 48, 'class': 'h5fan-axis-name'
	}, [ _('Temperature (°C)') ]));

	var legend = svgEl('g', { 'class': 'h5fan-legend' }, [
		svgEl('g', { transform: 'translate(44, 462)' }, [
			svgEl('rect', { x: 0, y: 0, width: 166, height: 36, rx: 10, 'class': 'h5fan-legend-pill is-green' }),
			svgEl('line', { x1: 14, y1: 18, x2: 32, y2: 18, stroke: '#10b981', 'stroke-width': '3', 'stroke-linecap': 'round' }),
			svgEl('circle', { cx: 23, cy: 18, r: 3.5, fill: '#ffffff', stroke: '#10b981', 'stroke-width': '2' }),
			svgEl('text', { x: 40, y: 23, 'class': 'h5fan-legend-text is-green' }, [ _('Solid green: the control function') ])
		]),
		svgEl('g', { transform: 'translate(224, 462)' }, [
			svgEl('rect', { x: 0, y: 0, width: 192, height: 36, rx: 10, 'class': 'h5fan-legend-pill is-red' }),
			svgEl('line', { x1: 14, y1: 18, x2: 34, y2: 18, stroke: '#ef4444', 'stroke-width': '2.5', 'stroke-dasharray': '5 3', 'stroke-linecap': 'round' }),
			svgEl('text', { x: 42, y: 23, 'class': 'h5fan-legend-text is-red' }, [ _('Red dashed: kernel safety floor') ])
		]),
		svgEl('g', { transform: 'translate(430, 462)' }, [
			svgEl('rect', { x: 0, y: 0, width: 166, height: 36, rx: 10, 'class': 'h5fan-legend-pill is-blue' }),
			svgEl('line', { x1: 14, y1: 18, x2: 34, y2: 18, stroke: '#3b82f6', 'stroke-width': '2.5', 'stroke-dasharray': '3 3', 'stroke-linecap': 'round' }),
			svgEl('text', { x: 42, y: 23, 'class': 'h5fan-legend-text is-blue' }, [ _('Blue dashed: target output') ])
		]),
		svgEl('g', { transform: 'translate(610, 462)' }, [
			svgEl('rect', { x: 0, y: 0, width: 192, height: 36, rx: 10, 'class': 'h5fan-legend-pill is-marker' }),
			svgEl('circle', { cx: 24, cy: 18, r: 5.5, fill: '#ffffff', stroke: '#3b82f6', 'stroke-width': '3' }),
			svgEl('text', { x: 40, y: 23, 'class': 'h5fan-legend-text is-marker' }, [ _('Marker: current control temperature') ])
		])
	]);

	var svg = svgEl('svg', {
		viewBox: '0 0 880 540', 'class': 'h5fan-chart', role: 'img',
		preserveAspectRatio: 'xMidYMid meet', 'aria-label': _('Fan curve')
	}, [
		svgEl('title', {}, [ _('Temperature to fan output curve') ]),
		svgEl('desc', {}, [ _('Every point on the line is the output the controller applies at that temperature.') ]),
		svgEl('defs', {}, [
			svgEl('linearGradient', { id: 'h5fan-curve-grad', x1: '0%', y1: '100%', x2: '100%', y2: '0%' }, [
				svgEl('stop', { offset: '0%', 'stop-color': '#10b981' }),
				svgEl('stop', { offset: '50%', 'stop-color': '#059669' }),
				svgEl('stop', { offset: '100%', 'stop-color': '#0d9488' })
			]),
			svgEl('linearGradient', { id: 'h5fan-area-grad', x1: '0%', y1: '0%', x2: '0%', y2: '100%' }, [
				svgEl('stop', { offset: '0%', 'stop-color': '#10b981', 'stop-opacity': '0.28' }),
				svgEl('stop', { offset: '65%', 'stop-color': '#10b981', 'stop-opacity': '0.08' }),
				svgEl('stop', { offset: '100%', 'stop-color': '#10b981', 'stop-opacity': '0.0' })
			]),
			svgEl('linearGradient', { id: 'h5fan-beam-grad', x1: '0%', y1: '0%', x2: '0%', y2: '100%' }, [
				svgEl('stop', { offset: '0%', 'stop-color': '#3b82f6', 'stop-opacity': '0.9' }),
				svgEl('stop', { offset: '50%', 'stop-color': '#3b82f6', 'stop-opacity': '0.5' }),
				svgEl('stop', { offset: '100%', 'stop-color': '#3b82f6', 'stop-opacity': '0.15' })
			]),
			svgEl('filter', { id: 'h5fan-glow', x: '-5%', y: '-10%', width: '110%', height: '125%' }, [
				svgEl('feDropShadow', { dx: '0', dy: '5', stdDeviation: '4', 'flood-color': '#059669', 'flood-opacity': '0.22' })
			]),
			svgEl('filter', { id: 'h5fan-node-shadow', x: '-30%', y: '-30%', width: '160%', height: '160%' }, [
				svgEl('feDropShadow', { dx: '0', dy: '2', stdDeviation: '3', 'flood-color': '#0f172a', 'flood-opacity': '0.12' })
			]),
			svgEl('g', { id: 'h5fan-point' }, [
				svgEl('circle', { cx: 0, cy: 0, r: 6, fill: '#ffffff', stroke: '#10b981', 'stroke-width': '3', filter: 'url(#h5fan-node-shadow)' })
			])
		]),
		svgEl('rect', { x: left, y: top, width: plotWidth, height: bottom - top, 'class': 'h5fan-plot-bg' }),
		grid, axis, area, floorLine, floorPill, floorLabel, targetLine, targetPill, targetLabel,
		line, cursor, markerPulse, marker, tooltip, nodes, error, legend
	]);

	svg.h5update = function(data) {
		var points = data.points || [];
		var usable = !data.error && points.length >= 2;

		error.textContent = data.error || '';
		show(error, !!data.error);
		show(line, usable);
		show(area, usable);

		if (!usable) {
			clear(nodes);
			show(marker, false);
			show(markerPulse, false);
			show(cursor, false);
			show(targetLine, false);
			show(targetPill, false);
			show(targetLabel, false);
			show(floorLine, false);
			show(floorPill, false);
			show(floorLabel, false);
			show(tooltip, false);
			svg.setAttribute('aria-label', _('Curve unavailable: %s').format(data.error || ''));
			return;
		}

		var path = '', i;

		for (i = minTemp; i <= maxTemp; i++)
			path += '%s%s %s'.format(i === minTemp ? 'M' : 'L', xFor(i).toFixed(2), yFor(curvePercentAt(points, i)).toFixed(2));

		line.setAttribute('d', path);
		area.setAttribute('d', '%s L%s %s L%s %s Z'.format(
			path, right.toFixed(2), bottom.toFixed(2), left.toFixed(2), bottom.toFixed(2)));

		clear(nodes);

		points.forEach(function(point) {
			var label = '%d°:%d%%'.format(point.temp, point.percent);
			nodes.appendChild(svgEl('g', { transform: 'translate(%s, %s)'.format(
				xFor(point.temp).toFixed(2), yFor(point.percent).toFixed(2)) }, [
				svgEl('use', { href: '#h5fan-point' }),
				svgEl('rect', { x: -32, y: -32, width: 64, height: 20, rx: 5, 'class': 'h5fan-node-pill' }),
				svgEl('text', { x: 0, y: -18, 'class': 'h5fan-node-text', 'text-anchor': 'middle' }, [ label ])
			]));
		});

		if (known(data.targetPercent)) {
			targetLine.setAttribute('y1', yFor(data.targetPercent));
			targetLine.setAttribute('y2', yFor(data.targetPercent));
			targetPill.setAttribute('y', (yFor(data.targetPercent) - 24).toFixed(2));
			targetLabel.setAttribute('y', (yFor(data.targetPercent) - 11.5).toFixed(2));
			targetLabel.textContent = _('Target %d%%').format(data.targetPercent);
			show(targetLine, true);
			show(targetPill, true);
			show(targetLabel, true);
		} else {
			show(targetLine, false);
			show(targetPill, false);
			show(targetLabel, false);
		}

		if (known(data.floorPercent) && data.floorPercent > 0) {
			floorLine.setAttribute('y1', yFor(data.floorPercent));
			floorLine.setAttribute('y2', yFor(data.floorPercent));
			floorPill.setAttribute('y', (yFor(data.floorPercent) + 6).toFixed(2));
			floorLabel.setAttribute('y', (yFor(data.floorPercent) + 18.5).toFixed(2));
			floorLabel.textContent = _('Floor %d%%').format(data.floorPercent);
			show(floorLine, true);
			show(floorPill, true);
			show(floorLabel, true);
		} else {
			show(floorLine, false);
			show(floorPill, false);
			show(floorLabel, false);
		}

		if (data.control && known(data.control.temp_c)) {
			var at = curvePercentAt(points, data.control.temp_c);
			var cursorX = xFor(data.control.temp_c);
			cursor.setAttribute('x1', cursorX);
			cursor.setAttribute('x2', cursorX);
			show(cursor, true);
			marker.setAttribute('cx', cursorX);
			marker.setAttribute('cy', yFor(at));
			markerPulse.setAttribute('cx', cursorX);
			markerPulse.setAttribute('cy', yFor(at));
			show(marker, true);
			show(markerPulse, true);
			tooltip.setAttribute('transform', 'translate(%s, %s)'.format(
				clamp(cursorX, left + 56, right - 56).toFixed(2), (top - 38).toFixed(2)));
			tooltipText.textContent = '%d °C · %d%%'.format(data.control.temp_c, at);
			show(tooltip, true);
			svg.setAttribute('aria-label', _('%1$s at %2$s gives %3$d%% output').format(
				data.control.sensor_label || _('control temperature'),
				formatTemp(data.control.temp_c), at));
		} else {
			show(cursor, false);
			show(marker, false);
			show(markerPulse, false);
			show(tooltip, false);
			svg.setAttribute('aria-label', _('Fan curve'));
		}
	};

	return svg;
}

function stateName(state) {
	if (state === 'ok')
		return 'Valid';

	if (state === 'stale')
		return 'Stale';

	if (state === 'invalid')
		return 'Invalid value';

	if (state === 'failed')
		return 'Read failed';

	return 'Not present';
}

function healthName(health) {
	if (health === 'failsafe')
		return _('Failsafe cooling active');

	if (health === 'stale')
		return _('Status is stale');

	if (health === 'error')
		return _('Controller error');

	if (health === 'stopped')
		return _('Controller not running');

	return _('Running normally');
}

function healthTone(health) {
	if (health === 'error' || health === 'stopped')
		return 'fail';

	if (health === 'stale' || health === 'failsafe')
		return 'warn';

	return 'ok';
}

function reasonName(reason) {
	var base = String(reason || ''), raised = base.indexOf('+kernel-floor') > -1, text;

	base = base.replace('+kernel-floor', '');

	if (base === 'auto')
		text = _('Automatic curve');
	else if (base === 'auto-down-delay')
		text = _('Holding output before slowing down');
	else if (base === 'manual')
		text = _('Manual request');
	else if (base === 'kernel')
		text = _('Kernel protection');
	else if (base === 'sensor-failsafe')
		text = _('Sensor failsafe');
	else if (base === 'curve-failsafe')
		text = _('Curve failsafe');
	else if (base === 'pwm-missing')
		text = _('Fan control node not found');
	else if (base === 'pwm-write-failed')
		text = _('Writing the fan output failed');
	else if (base === 'no-snapshot')
		text = _('No cycle published yet');
	else
		text = base || _('Unknown');

	return raised ? _('%s · raised by the kernel floor').format(text) : text;
}

/* ---------------------------------------------------------------- the view */

return view.extend({
	status: null,
	fetchError: null,
	values: null,
	original: null,
	widgets: null,
	charts: null,
	tabId: 'overview',

	title: _('Fan Control'),

	/* ------------------------------------------------------------- loading */

	fetchStatus: function() {
		return fs.exec(STATUS_CMD, [ 'json' ]).then(L.bind(function(result) {
			if (result && result.code !== 0)
				throw new Error(_('The status command exited with code %s').format(result.code));

			var payload = JSON.parse(String(result.stdout || ''));

			if (!payload || typeof payload !== 'object' || !payload.controller || !payload.settings)
				throw new Error(_('The status reply does not match the expected contract'));

			this.fetchError = null;
			this.status = payload;
		}, this)).catch(L.bind(function(err) {
			this.fetchError = (err && err.message) ? err.message : String(err);
		}, this));
	},

	load: function() {
		return Promise.all([
			uci.load(CFG).catch(L.bind(function(err) {
				this.fetchError = (err && err.message) ? err.message : String(err);
			}, this)),
			this.fetchStatus()
		]).then(L.bind(function() {
			this.readStoredConfig();
			return null;
		}, this));
	},

	readStoredConfig: function() {
		var values = {}, original = {}, key;

		for (key in OPTIONS) {
			var stored = null;

			try {
				stored = uci.get(CFG, SID, key);
			} catch (err) {
				stored = null;
			}

			if (!known(stored))
				stored = known(this.settings()[key]) ? String(this.settings()[key]) : OPTIONS[key].def;

			if (key === 'enabled')
				stored = (stored === '0' || stored === false) ? '0' : '1';

			values[key] = String(stored);
			original[key] = String(stored);
		}

		this.values = values;
		this.original = original;
	},

	settings: function() {
		return (this.status && this.status.settings) || {};
	},

	fanState: function() {
		return (this.status && this.status.fan) || {};
	},

	thermal: function() {
		return (this.status && this.status.thermal) || {};
	},

	protections: function() {
		return this.thermal().protections || {};
	},

	controller: function() {
		return (this.status && this.status.controller) || {};
	},

	sensorById: function(id) {
		var list = (this.status && this.status.sensors) || [], i;

		for (i = 0; i < list.length; i++) {
			if (list[i].id === id)
				return list[i];
		}

		return null;
	},

	/* ------------------------------------------------------------- styling */

	styleNode: function() {
		/*
		 * The root pins its own stacking context on purpose: a wide card that
		 * filtered or transformed would create a layer above the theme's sticky
		 * header and hide the navigation. Nothing here uses backdrop-filter.
		 */
		return E('style', { 'type': 'text/css' }, [ [
			'.h5fan{position:relative;z-index:0;isolation:isolate;display:block;width:100%;',
			'--h5fan-green:#2ea97a;--h5fan-blue:#3f8ad6;--h5fan-amber:#d9963f;--h5fan-red:#d65b5b;',
			'--h5fan-line:rgba(127,127,127,.22);color:var(--text-color-high,#222)}',
			'.h5fan *,.h5fan *:*{box-sizing:border-box}',
			'.h5fan-card,.h5fan-panel,.h5fan-kv,.h5fan-node,.h5fan-banner,.h5fan-tabs{border:1px solid var(--h5fan-line);border-radius:12px;background:var(--background-color-high,#fff)}',
			'.h5fan-head{display:flex;flex-wrap:wrap;align-items:center;gap:10px;margin:0 0 12px}',
			'.h5fan-head h2{margin:0;font-size:20px;line-height:1.25}',
			'.h5fan-head-sub{margin:0;font-size:12.5px;color:var(--text-color-medium,#666);flex:1 1 100%}',
			'.h5fan-health{display:inline-flex;align-items:center;gap:7px;padding:5px 11px;border-radius:999px;font-size:12px;font-weight:600;background:rgba(46,169,122,.12);color:var(--h5fan-green)}',
			'.h5fan-health:before{content:"";width:8px;height:8px;border-radius:50%;background:currentColor}',
			'.h5fan-health.is-warn{background:rgba(217,150,63,.15);color:var(--h5fan-amber)}',
			'.h5fan-health.is-fail{background:rgba(214,91,91,.15);color:var(--h5fan-red)}',
			'.h5fan-head-actions{display:flex;flex-wrap:wrap;gap:7px;margin-left:auto}',
			'.h5fan-banner{display:none;margin:0 0 12px;padding:11px 13px;font-size:12.5px;line-height:1.5;color:var(--h5fan-red);background:rgba(214,91,91,.08);border-left:3px solid var(--h5fan-red)}',
			'.h5fan-banner.is-shown{display:block}',
			'.h5fan-tabs{display:flex;flex-wrap:wrap;gap:6px;margin:0 0 14px;padding:5px}',
			'.h5fan-tab{position:relative;appearance:none;border:0;border-radius:9px;padding:8px 14px;font-size:13.5px;font-weight:600;color:var(--text-color-medium,#555);background:transparent;cursor:pointer;white-space:nowrap}',
			'.h5fan-tab:hover{background:rgba(127,127,127,.1);color:var(--text-color-high,#222)}',
			'.h5fan-tab[aria-selected="true"]{background:var(--h5fan-green);color:#fff}',
			'.h5fan-tab:focus-visible{outline:2px solid var(--h5fan-blue);outline-offset:2px}',
			'.h5fan-tab[data-dirty="1"]:after{content:"";position:absolute;top:5px;right:5px;width:7px;height:7px;border-radius:50%;background:var(--h5fan-amber)}',
			'.h5fan-pane{display:grid;grid-template-columns:repeat(12,minmax(0,1fr));gap:14px;align-items:stretch;margin:0 0 14px}',
			'.h5fan-pane[hidden]{display:none}',
			'.h5fan-panel{padding:14px;display:flex;flex-direction:column;gap:12px;min-width:0}',
			'.h5fan-panel h3,.h5fan-panel h4{margin:0;font-size:14px;font-weight:650}',
			'.h5fan-panel-sub{margin:-6px 0 0;font-size:11.5px;line-height:1.5;color:var(--text-color-medium,#666)}',
			'.h5fan-span12{grid-column:span 12}.h5fan-span8{grid-column:span 8}.h5fan-span7{grid-column:span 7}',
			'.h5fan-span6{grid-column:span 6}.h5fan-span5{grid-column:span 5}.h5fan-span4{grid-column:span 4}',
			'@media(max-width:1000px){.h5fan-span8,.h5fan-span7,.h5fan-span6,.h5fan-span5,.h5fan-span4{grid-column:span 12}}',
			'@media(max-width:640px){.h5fan-pane{gap:10px}.h5fan-head-actions{margin-left:0;width:100%}.h5fan-head h2{font-size:18px}}',
			'.h5fan-stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(124px,1fr));gap:10px}',
			'.h5fan-kv{padding:11px 12px;display:flex;flex-direction:column;gap:4px;min-width:0}',
			'.h5fan-kv-label{font-size:11.5px;color:var(--text-color-medium,#666)}',
			'.h5fan-kv-value{font-size:19px;font-weight:650;word-break:break-word}',
			'.h5fan-kv-value.is-warn{color:var(--h5fan-amber)}.h5fan-kv-value.is-fail{color:var(--h5fan-red)}',
			'.h5fan-kv-hint{font-size:11px;color:var(--text-color-low,#888);word-break:break-word}',
			'.h5fan-badges{display:flex;flex-wrap:wrap;gap:7px}',
			'.h5fan-badge{padding:4px 9px;border-radius:8px;font-size:11.5px;background:rgba(63,138,214,.12);color:var(--h5fan-blue);white-space:nowrap}',
			'.h5fan-badge.is-warn{background:rgba(217,150,63,.15);color:var(--h5fan-amber)}',
			'.h5fan-badge.is-fail{background:rgba(214,91,91,.15);color:var(--h5fan-red)}',
			'.h5fan-tag{display:inline-block;padding:2px 8px;border-radius:999px;font-size:11px;font-weight:600;background:rgba(127,127,127,.13)}',
			'.h5fan-tag.is-ok{background:rgba(46,169,122,.14);color:var(--h5fan-green)}',
			'.h5fan-tag.is-warn{background:rgba(217,150,63,.16);color:var(--h5fan-amber)}',
			'.h5fan-tag.is-fail{background:rgba(214,91,91,.16);color:var(--h5fan-red)}',
			'.h5fan-rows{display:flex;flex-direction:column;gap:7px;margin:0}',
			'.h5fan-row{display:flex;flex-wrap:wrap;align-items:baseline;gap:8px;font-size:12.5px;padding-bottom:6px;border-bottom:1px dashed rgba(127,127,127,.18)}',
			'.h5fan-row:last-child{border-bottom:0;padding-bottom:0}',
			'.h5fan-row-key{flex:1 1 42%;min-width:0;color:var(--text-color-medium,#666);word-break:break-word}',
			'.h5fan-row-val{flex:1 1 50%;min-width:0;text-align:right;font-weight:600;word-break:break-word}',
			'.h5fan-row-val.is-ok{color:var(--h5fan-green)}.h5fan-row-val.is-warn{color:var(--h5fan-amber)}.h5fan-row-val.is-fail{color:var(--h5fan-red)}',
			'.h5fan-note{padding:10px 12px;font-size:12px;line-height:1.55;color:var(--text-color-medium,#555);background:rgba(63,138,214,.07);border-left:3px solid var(--h5fan-blue);border-radius:8px;margin:0}',
			'.h5fan-note.is-warn{background:rgba(217,150,63,.1);border-left-color:var(--h5fan-amber)}',
			'.h5fan-note.is-fail{background:rgba(214,91,91,.1);border-left-color:var(--h5fan-red)}',
			'.h5fan-tablewrap{overflow-x:auto}',
			'.h5fan-events-wrap{max-height:320px;overflow-y:auto}',
			'.h5fan-table{width:100%;border-collapse:collapse;font-size:12.5px}',
			'.h5fan-table th,.h5fan-table td{padding:7px 8px;text-align:left;border-bottom:1px solid rgba(127,127,127,.15);vertical-align:top;word-break:break-word}',
			'.h5fan-table th{font-weight:650;color:var(--text-color-medium,#666);white-space:nowrap}',
			'.h5fan-table tr:last-child td{border-bottom:0}',
			'.h5fan-fan{display:block;width:100%;max-width:250px;height:auto;margin:0 auto}',
			'.h5fan-fan-panel{justify-content:center}',
			'.h5fan-rotor{transform-origin:210px 195px;animation:h5fan-spin 2.8s linear infinite}',
			'.h5fan-airflow{transform-origin:210px 195px;animation:h5fan-spin-rev 6s linear infinite}',
			'.h5fan-dot{animation:h5fan-pulse 2s ease-in-out infinite}',
			'.h5fan-progress-value{stroke:url(#h5fan-progress-grad)}',
			'.h5fan-fan.is-fault .h5fan-progress-value{stroke:var(--h5fan-red)}',
			'.h5fan-fan.is-stopped .h5fan-rotor,.h5fan-fan.is-unknown .h5fan-rotor{animation-play-state:paused;opacity:.55;filter:grayscale(.85)}',
			'.h5fan-fan.is-fault .h5fan-rotor{animation:h5fan-wobble 1.1s ease-in-out infinite}',
			'.h5fan-capsule-label{font-size:15px;fill:var(--text-color-medium,#64748b)}',
			'.h5fan-capsule-value{font-size:17px;font-weight:700;fill:var(--text-color-high,#0f172a)}',
			'@keyframes h5fan-spin{to{transform:rotate(360deg)}}',
			'@keyframes h5fan-spin-rev{to{transform:rotate(-360deg)}}',
			'@keyframes h5fan-pulse{0%,100%{opacity:1}50%{opacity:.45}}',
			'@keyframes h5fan-wobble{0%,100%{transform:rotate(-7deg)}50%{transform:rotate(7deg)}}',
			'@media (prefers-reduced-motion:reduce){.h5fan .h5fan-rotor,.h5fan .h5fan-airflow,.h5fan .h5fan-dot{animation:none!important}}',
			'.h5fan-gauge{display:block;width:100%;max-width:132px;height:auto;margin:0 auto}',
			'.h5fan-gauge-track{fill:none;stroke:rgba(127,127,127,.15);stroke-width:11}',
			'.h5fan-gauge-value{fill:none;stroke:var(--h5fan-blue);stroke-width:11;stroke-linecap:round}',
			'.h5fan-gauge-value.is-ok{stroke:var(--h5fan-green)}.h5fan-gauge-value.is-warn{stroke:var(--h5fan-amber)}.h5fan-gauge-value.is-fail{stroke:var(--h5fan-red)}',
			'.h5fan-gauge-num{font-size:24px;font-weight:700;fill:var(--text-color-high,#222)}',
			'.h5fan-gauge-unit{font-size:10px;fill:var(--text-color-medium,#666)}',
			'.h5fan-chart{display:block;width:100%;height:auto}',
			'.h5fan-plot-bg{fill:#f8fafc;opacity:.6}',
			'.h5fan-grid-line{stroke:#e2e8f0;stroke-width:1}',
			'.h5fan-grid-line.is-dashed{stroke-dasharray:4 4}',
			'.h5fan-grid-line.is-faint{stroke:#f1f5f9}',
			'.h5fan-grid-line.is-edge{stroke:#cbd5e1;stroke-width:1.5}',
			'.h5fan-axis-line{stroke:#cbd5e1;stroke-width:1.5}',
			'.h5fan-tick{font-size:12.5px;font-weight:500;fill:#64748b}',
			'.h5fan-axis-name{font-size:13px;fill:#64748b}',
			'.h5fan-line-pill.is-floor{fill:#fef2f2;opacity:.9}',
			'.h5fan-line-pill.is-target{fill:#eff6ff;opacity:.9}',
			'.h5fan-line-pill-text{font-size:10.5px;font-weight:600}',
			'.h5fan-line-pill-text.is-floor{fill:#dc2626}',
			'.h5fan-line-pill-text.is-target{fill:#2563eb}',
			'.h5fan-node-pill{fill:#ffffff;stroke:#e2e8f0;stroke-width:1}',
			'.h5fan-node-text{font-size:12px;font-weight:600;fill:#334155}',
			'.h5fan-tooltip-box{fill:#0f172a}',
			'.h5fan-tooltip-text{font-size:13px;font-weight:700;fill:#ffffff}',
			'.h5fan-marker{fill:#ffffff;stroke:#3b82f6;stroke-width:4}',
			'.h5fan-marker-pulse{fill:#3b82f6;opacity:.75;transform-box:fill-box;transform-origin:center;animation:h5fan-radar 2.2s cubic-bezier(.16,1,.3,1) infinite}',
			'@keyframes h5fan-radar{0%{transform:scale(.8);opacity:.75}100%{transform:scale(2.5);opacity:0}}',
			'.h5fan-legend-pill.is-green{fill:#ecfdf5;stroke:#a7f3d0}',
			'.h5fan-legend-pill.is-red{fill:#fef2f2;stroke:#fecaca}',
			'.h5fan-legend-pill.is-blue{fill:#eff6ff;stroke:#bfdbfe}',
			'.h5fan-legend-pill.is-marker{fill:#f0f9ff;stroke:#bae6fd}',
			'.h5fan-legend-text{font-size:13px;font-weight:600}',
			'.h5fan-legend-text.is-green{fill:#065f46}',
			'.h5fan-legend-text.is-red{fill:#b91c1c}',
			'.h5fan-legend-text.is-blue{fill:#1d4ed8}',
			'.h5fan-legend-text.is-marker{fill:#0369a1}',
			'.h5fan-chart-error{font-size:13px;fill:var(--h5fan-red)}',
			'.h5fan-bars{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:10px}',
			'.h5fan-node{padding:10px 12px}',
			'.h5fan-bar{display:block;width:100%;height:auto}',
			'.h5fan-bar-track{fill:rgba(127,127,127,.12)}',
			'.h5fan-bar-hatched{fill:none;stroke:rgba(127,127,127,.4);stroke-width:1;stroke-dasharray:4 4}',
			'.h5fan-bar-fill{fill:var(--h5fan-green)}',
			'.h5fan-bar-fill.is-watch{fill:var(--h5fan-amber)}.h5fan-bar-fill.is-hot{fill:#df6a3d}.h5fan-bar-fill.is-critical{fill:var(--h5fan-red)}',
			'.h5fan-bar-fill.is-unknown,.h5fan-bar-fill.is-stale,.h5fan-bar-fill.is-bad{fill:rgba(127,127,127,.25)}',
			'.h5fan-bar-name{font-size:12px;fill:var(--text-color-medium,#666)}',
			'.h5fan-bar-value{font-size:13px;font-weight:700;fill:var(--text-color-high,#222)}',
			'.h5fan-bar-note{font-size:10.5px;fill:var(--text-color-low,#888)}',
			'.h5fan-bar-tick{stroke:rgba(127,127,127,.5);stroke-width:1}',
			'.h5fan-fields{display:flex;flex-direction:column;gap:0}',
			'.h5fan-field{display:grid;grid-template-columns:minmax(0,38%) minmax(0,62%);gap:10px;align-items:start;padding:10px 0;border-bottom:1px solid rgba(127,127,127,.13)}',
			'.h5fan-field:last-child{border-bottom:0}',
			'.h5fan-field-label{font-size:13px;font-weight:600;line-height:1.4}',
			'.h5fan-field-label small{display:block;margin-top:3px;font-weight:400;font-size:11.5px;line-height:1.5;color:var(--text-color-medium,#666)}',
			'.h5fan-field-body{display:flex;flex-direction:column;gap:6px;min-width:0}',
			'.h5fan-field-body select,.h5fan-field-body input[type=text],.h5fan-field-body input[type=number]{width:100%;font-size:13px}',
			'.h5fan-unit{font-size:11.5px;color:var(--text-color-low,#888)}',
			'.h5fan-error{font-size:11.5px;color:var(--h5fan-red);min-height:15px}',
			'@media(max-width:640px){.h5fan-field{grid-template-columns:minmax(0,100%);gap:6px}}',
			'.h5fan-switch{display:inline-flex;align-items:center;gap:9px;cursor:pointer;font-size:13px}',
			'.h5fan-switch input{position:absolute;opacity:0;pointer-events:none}',
			'.h5fan-switch .h5fan-track{position:relative;width:46px;height:24px;border-radius:999px;background:rgba(127,127,127,.3);transition:background .15s ease;flex:0 0 auto}',
			'.h5fan-switch .h5fan-track:after{content:"";position:absolute;top:3px;left:3px;width:18px;height:18px;border-radius:50%;background:#fff;transition:transform .15s ease}',
			'.h5fan-switch input:checked+.h5fan-track{background:var(--h5fan-green)}',
			'.h5fan-switch input:checked+.h5fan-track:after{transform:translateX(22px)}',
			'.h5fan-switch input:focus-visible+.h5fan-track{outline:2px solid var(--h5fan-blue);outline-offset:2px}',
			'.h5fan-seg{display:flex;flex-wrap:wrap;gap:6px}',
			'.h5fan-seg label{position:relative;padding:7px 12px;border:1px solid var(--h5fan-line);border-radius:9px;font-size:12.5px;cursor:pointer;background:var(--background-color-high,#fff)}',
			'.h5fan-seg input{position:absolute;opacity:0;pointer-events:none}',
			'.h5fan-seg label.is-on{background:var(--h5fan-blue);border-color:var(--h5fan-blue);color:#fff;font-weight:600}',
			'.h5fan-slider{display:flex;align-items:center;gap:10px;flex-wrap:wrap}',
			'.h5fan-slider input[type=range]{flex:1 1 180px;min-width:140px}',
			'.h5fan-slider input[type=number]{flex:0 0 auto;width:92px}',
			'.h5fan-actions{display:flex;flex-wrap:wrap;gap:8px;align-items:center}',
			'.h5fan-nodes{display:flex;flex-direction:column;gap:7px}',
			'.h5fan-noderow{display:grid;grid-template-columns:minmax(0,1fr) minmax(0,1fr) auto;gap:8px;align-items:end}',
			'.h5fan-noderow label{font-size:11px;color:var(--text-color-medium,#666)}',
			'.h5fan-noderow input{width:100%}',
			'.h5fan-empty{padding:14px;font-size:12.5px;color:var(--text-color-medium,#666);text-align:center}'
		].join('') ]);
	},

	/* ------------------------------------------------------------- chrome */

	head: function() {
		var health = E('span', { 'class': 'h5fan-health' }, [ _('Loading…') ]);
		var actions = E('div', { 'class': 'h5fan-head-actions' });

		this.headHealth = health;

		[
			[ _('Save'), 'btn', L.bind(this.handleSave, this) ],
			[ _('Save & Apply'), 'btn cbi-button-positive', L.bind(this.handleSaveApply, this) ],
			[ _('Revert'), 'btn', L.bind(this.handleReset, this) ]
		].forEach(L.bind(function(spec) {
			actions.appendChild(E('button', {
				'class': spec[1], type: 'button', click: spec[2]
			}, [ spec[0] ]));
		}, this));

		return E('div', { 'class': 'h5fan-head' }, [
			E('h2', {}, [ _('H5000M cooling management') ]),
			health,
			actions,
			E('p', { 'class': 'h5fan-head-sub' }, [
				_('Balances cooling against noise without touching the kernel CPU throttling, hot or critical protection.')
			])
		]);
	},

	banner: function() {
		this.bannerNode = E('div', { 'class': 'h5fan-banner', role: 'alert' }, [ '' ]);
		return this.bannerNode;
	},

	tabBar: function() {
		var bar = E('div', { 'class': 'h5fan-tabs', role: 'tablist', 'aria-label': _('Sections') });

		this.tabButtons = [];

		TABS.forEach(L.bind(function(tab) {
			var button = E('button', {
				type: 'button', 'class': 'h5fan-tab', role: 'tab',
				'data-h5tab': tab.id, 'aria-selected': 'false',
				click: L.bind(function() {
					this.selectTab(tab.id);
				}, this)
			}, [ _(tab.label) ]);

			this.tabButtons.push({ id: tab.id, node: button });
			bar.appendChild(button);
		}, this));

		return bar;
	},

	restoreTab: function() {
		var hash = String(window.location.hash || '').replace('#', ''), stored;

		if (TABS.some(function(tab) {
			return tab.id === hash;
		}))
			return hash;

		try {
			stored = window.localStorage.getItem(TAB_KEY);
		} catch (err) {
			stored = null;
		}

		return TABS.some(function(tab) {
			return tab.id === stored;
		}) ? stored : 'overview';
	},

	selectTab: function(id, remember) {
		this.tabId = TABS.some(function(tab) {
			return tab.id === id;
		}) ? id : 'overview';

		this.tabButtons.forEach(L.bind(function(tab) {
			tab.node.setAttribute('aria-selected', tab.id === this.tabId ? 'true' : 'false');
		}, this));

		TABS.forEach(L.bind(function(tab) {
			var pane = this.panes[tab.id];

			if (pane)
				pane.hidden = tab.id !== this.tabId;
		}, this));

		if (remember === false)
			return;

		try {
			window.localStorage.setItem(TAB_KEY, this.tabId);
		} catch (err) {
			/* Storage can be unavailable in private mode; the hash still works. */
		}

		if (window.location.hash !== '#%s'.format(this.tabId))
			window.location.hash = '#%s'.format(this.tabId);
	},

	/* ------------------------------------------------------------ widgets */

	field: function(key) {
		var spec = OPTIONS[key], body = E('div', { 'class': 'h5fan-field-body' });
		var wrap = E('div', { 'class': 'h5fan-field' }, [
			E('div', { 'class': 'h5fan-field-label' }, [
				_(spec.label),
				E('small', {}, [ _(spec.hint) ])
			]),
			body
		]);

		this.widgetInput = this.widgetInput || {};
		this.widgetError = this.widgetError || {};

		if (spec.kind === 'switch') {
			var checkbox = E('input', {
				type: 'checkbox', checked: this.values[key] === '1'
			});

			checkbox.addEventListener('change', L.bind(function(ev) {
				this.values[key] = ev.target.checked ? '1' : '0';
				this.afterChange(key);
			}, this));

			body.appendChild(E('label', { 'class': 'h5fan-switch' }, [
				checkbox, E('i', { 'class': 'h5fan-track' }), E('span', {}, [ _('Enabled') ])
			]));
			this.widgetInput[key] = checkbox;
		} else if (spec.kind === 'select') {
			var select = E('select', { 'class': 'cbi-dropdown' });

			spec.choices.forEach(function(choice) {
				var option = E('option', { value: choice[0] }, [ _(choice[1]) ]);

				if (choice[0] === this.values[key])
					option.selected = true;

				select.appendChild(option);
			}, this);

			select.addEventListener('change', L.bind(function(ev) {
				this.values[key] = ev.target.value;
				this.afterChange(key);
			}, this));

			body.appendChild(select);
			this.widgetInput[key] = select;
		} else if (spec.kind === 'slider') {
			var range = E('input', {
				type: 'range', min: String(spec.min), max: String(spec.max), step: '1',
				value: String(this.values[key])
			});
			var box = E('input', {
				type: 'number', 'class': 'cbi-input-text', min: String(spec.min),
				max: String(spec.max), step: '1', value: String(this.values[key])
			});

			function sync(from, to) {
				from.addEventListener('input', L.bind(function(ev) {
					var value = ev.target.value;
					this.values[key] = value;
					to.value = value;
					this.afterChange(key);
				}, this));
			}

			sync(range, box);
			sync(box, range);

			body.appendChild(E('div', { 'class': 'h5fan-slider' }, [ range, box ]));
			body.appendChild(E('span', { 'class': 'h5fan-unit' }, [ _('0 is stopped, 255 is full speed.') ]));
			this.widgetInput[key] = box;
			this.widgetRange = this.widgetRange || {};
			this.widgetRange[key] = range;
		} else {
			var input = E('input', {
				type: 'text', 'class': 'cbi-input-text', value: String(this.values[key])
			});

			input.addEventListener('input', L.bind(function(ev) {
				this.values[key] = ev.target.value;
				this.afterChange(key);
			}, this));

			body.appendChild(input);

			if (spec.unit)
				body.appendChild(E('span', { 'class': 'h5fan-unit' }, [ _('Allowed') + ': ' + spec.unit ]));

			this.widgetInput[key] = input;
		}

		if (spec.kind === 'curve') {
			body.appendChild(E('div', { 'class': 'h5fan-note is-warn', hidden: true }, [ '' ]));
			this.widgetError[key] = body.lastChild;
		}

		this.fields = this.fields || {};
		this.fields[key] = wrap;
		return wrap;
	},

	afterChange: function(key) {
		this.validateOption(key, true);
		this.markDirty();

		if (key === 'curve_custom' || key === 'curve')
			this.syncCurvePreview();

		if (key === 'manual_pwm' || key === 'mode')
			this.paintLiveState();
	},

	validateOption: function(key, show) {
		var spec = OPTIONS[key], value = this.values[key], message = null;

		if (spec.kind === 'switch')
			message = (value === '0' || value === '1') ? null : _('Choose enabled or disabled.');
		else if (spec.kind === 'select')
			message = spec.choices.some(function(choice) {
				return choice[0] === value;
			}) ? null : _('Pick one of the listed values.');
		else if (spec.kind === 'curve')
			message = parseCurve(value).error;
		else {
			var parsed = num(value);

			if (parsed === null || String(parsed) !== String(value).trim() || parsed < spec.min || parsed > spec.max)
				message = _('%1$s must be a whole number between %2$s and %3$s.').format(_(spec.label), spec.min, spec.max);
		}

		if (show !== false && this.widgetError && this.widgetError[key]) {
			this.widgetError[key].hidden = !message;
			this.widgetError[key].textContent = message || '';
		}

		if (show !== false && this.widgetInput && this.widgetInput[key])
			this.widgetInput[key].setAttribute('aria-invalid', message ? 'true' : 'false');

		return message;
	},

	validateAll: function() {
		var problems = [], key;

		for (key in OPTIONS) {
			var message = this.validateOption(key, true);

			if (message)
				problems.push('%s: %s'.format(_(OPTIONS[key].label), message));
		}

		return problems;
	},

	markDirty: function() {
		var dirtyKeys = this.dirtyKeys(), byTab = {}, key;

		TABS.forEach(function(tab) {
			byTab[tab.id] = false;
		});

		for (key in OPTIONS) {
			if (this.values[key] !== this.original[key])
				byTab[OPTIONS[key].tab] = true;
		}

		this.tabButtons.forEach(function(tab) {
			tab.node.setAttribute('data-dirty', byTab[tab.id] ? '1' : '0');
		});

		this.dirty = dirtyKeys;
	},

	dirtyKeys: function() {
		var keys = [], key;

		for (key in OPTIONS) {
			if (this.values[key] !== this.original[key])
				keys.push(key);
		}

		return keys;
	},

	/* --------------------------------------------------------------- panes */

	buildPanes: function() {
		this.panes = {};

		TABS.forEach(L.bind(function(tab) {
			this.panes[tab.id] = E('div', { 'class': 'h5fan-pane', 'data-h5tab': tab.id, hidden: true });
		}, this));

		this.charts = {
			fan: mountFanDiagram(),
			output: mountGauge('Applied output'),
			target: mountGauge('Target output'),
			curve: mountCurveChart()
		};

		this.buildOverview();
		this.buildControl();
		this.buildCurve();
		this.buildMonitor();
		this.buildSystem();
		this.buildDiagnostics();

		return TABS.map(L.bind(function(tab) {
			return this.panes[tab.id];
		}, this));
	},

	buildOverview: function() {
		var pane = this.panes.overview, stats = {};

		this.stats = stats;

		[
			[ 'reason', _('Control reason') ],
			[ 'control', _('Control temperature') ],
			[ 'requested', _('Requested') ],
			[ 'target', _('Target') ],
			[ 'actual', _('Register readback') ],
			[ 'floor', _('Kernel floor') ]
		].forEach(function(spec) {
			var value = E('div', { 'class': 'h5fan-kv-value' }, [ '\u2014' ]);
			var hint = E('div', { 'class': 'h5fan-kv-hint' }, [ '' ]);

			stats[spec[0]] = {
				value: value, hint: hint,
				set: function(text, note, tone) {
					value.textContent = text;
					value.className = 'h5fan-kv-value%s'.format(tone ? ' is-%s'.format(tone) : '');
					hint.textContent = note || '';
				}
			};

			stats[spec[0]].node = E('div', { 'class': 'h5fan-kv' }, [
				E('div', { 'class': 'h5fan-kv-label' }, [ spec[1] ]), value, hint
			]);
		});

		this.overviewBadges = E('div', { 'class': 'h5fan-badges' });
		this.overviewRows = E('div', { 'class': 'h5fan-rows' });

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span3 h5fan-fan-panel' }, [
			E('h3', {}, [ _('Fan') ]),
			this.charts.fan
		]));

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span6' }, [
			E('h3', {}, [ _('Outputs') ]),
			E('div', { 'class': 'h5fan-bars' }, [
				E('div', { 'class': 'h5fan-node' }, [ this.charts.output ]),
				E('div', { 'class': 'h5fan-node' }, [ this.charts.target ])
			]),
			E('div', { 'class': 'h5fan-stats' }, [
				stats.requested.node, stats.actual.node, stats.floor.node
			])
		]));

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span3' }, [
			E('h3', {}, [ _('Status') ]),
			this.overviewBadges,
			this.overviewRows
		]));

		this.overviewBars = this.mountBars(E('div', { 'class': 'h5fan-bars' }));

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span12' }, [
			E('h3', {}, [ _('Temperatures') ]),
			E('div', { 'class': 'h5fan-stats' }, [
				stats.control.node, stats.reason.node
			]),
			this.overviewBars
		]));
	},

	/*
	 * Each pane needs its own chart instances: a DOM node can only live in one
	 * parent, so sharing one between the overview and the monitor tab would
	 * silently move it out of the first panel.
	 */
	mountBars: function(container) {
		this.allBars = this.allBars || [];

		SENSOR_SLOTS.forEach(L.bind(function(slot) {
			var chart = mountTempBar(slot);

			this.allBars.push({ id: slot.id, chart: chart });
			container.appendChild(E('div', { 'class': 'h5fan-node' }, [ chart ]));
		}, this));

		return container;
	},

	buildControl: function() {
		var pane = this.panes.control, fields = E('div', { 'class': 'h5fan-fields' });

		[ 'enabled', 'mode', 'curve', 'manual_pwm', 'start_pwm', 'start_boost_ms' ].forEach(L.bind(function(key) {
			fields.appendChild(this.field(key));
		}, this));

		this.controlRows = E('div', { 'class': 'h5fan-rows' });

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span6' }, [
			E('h3', {}, [ _('Fan control') ]), fields
		]));

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span6' }, [
			E('h3', {}, [ _('Live output') ]),
			this.controlRows,
			E('div', { 'class': 'h5fan-actions' }, [
				E('button', {
					'class': 'btn cbi-button-positive', type: 'button',
					click: L.bind(this.runApply, this)
				}, [ _('Apply now') ]),
				E('button', {
					'class': 'btn', type: 'button',
					click: L.bind(this.refresh, this)
				}, [ _('Refresh') ]),
				E('button', {
					'class': 'btn cbi-button-negative', type: 'button',
					click: L.bind(this.confirmRestart, this)
				}, [ _('Restart service') ])
			]),
			E('p', { 'class': 'h5fan-note' }, [
				_('Apply runs one control cycle immediately. The controller reloads its configuration by itself, so saving never restarts the service.')
			])
		]));
	},

	buildCurve: function() {
		var pane = this.panes.curve;

		this.nodeList = E('div', { 'class': 'h5fan-nodes' });
		this.nodeError = E('div', { 'class': 'h5fan-error' }, [ '' ]);

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span7' }, [
			E('h3', {}, [ _('Effective curve') ]),
			E('p', { 'class': 'h5fan-panel-sub' }, [
				_('The line is sampled once per degree with the same integer arithmetic the controller uses.')
			]),
			this.charts.curve
		]));

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span5' }, [
			E('h3', {}, [ _('Custom curve points') ]),
			E('p', { 'class': 'h5fan-panel-sub' }, [
				_('Editing here only changes the page. Use Save to store it, and the chart updates live.')
			]),
			E('div', { 'class': 'h5fan-seg', id: 'h5fan-preset-seg' }),
			this.nodeList,
			this.nodeError,
			E('div', { 'class': 'h5fan-actions' }, [
				E('button', {
					'class': 'btn', type: 'button', click: L.bind(function() {
						this.editorPoints.push({ temp: 25, percent: 0 });
						this.renderNodes();
					}, this)
				}, [ _('Add point') ]),
				E('button', {
					'class': 'btn cbi-button-negative', type: 'button', click: L.bind(function() {
						if (this.editorPoints.length > 2) {
							this.editorPoints.pop();
							this.renderNodes();
						}
					}, this)
				}, [ _('Remove last') ]),
				E('button', {
					'class': 'btn', type: 'button', click: L.bind(function() {
						this.resetCustomCurve();
					}, this)
				}, [ _('Restore default curve') ])
			]),
			E('p', { 'class': 'h5fan-note' }, [
				_('Temperatures must rise strictly and percentages stay within 0-100%. An invalid curve makes the controller run its failsafe output instead of stopping the fan.')
			])
		]));
	},

	buildMonitor: function() {
		var pane = this.panes.monitor;

		this.monitorTable = E('table', { 'class': 'h5fan-table' }, [
			E('tr', {}, [
				E('th', {}, [ _('Sensor') ]), E('th', {}, [ _('Value') ]), E('th', {}, [ _('State') ]),
				E('th', {}, [ _('Source') ]), E('th', {}, [ _('Updated') ]), E('th', {}, [ _('Detail') ])
			])
		]);

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span12' }, [
			E('h3', {}, [ _('Temperature monitor') ]),
			E('p', { 'class': 'h5fan-panel-sub' }, [
				_('Native thermal and hwmon readings win; the vendor radio query is only a bounded fallback. The colour bands are this project’s own reference ranges, not a vendor specification.')
			]),
			this.mountBars(E('div', { 'class': 'h5fan-bars' })),
			E('div', { 'class': 'h5fan-tablewrap' }, [ this.monitorTable ])
		]));
	},

	buildSystem: function() {
		var pane = this.panes.system, fields = E('div', { 'class': 'h5fan-fields' });

		[ 'temp_source', 'interval', 'hysteresis', 'down_delay', 'fail_pwm',
			'curve_silent', 'curve_balanced', 'curve_performance' ].forEach(L.bind(function(key) {
			fields.appendChild(this.field(key));
		}, this));

		this.systemRows = E('div', { 'class': 'h5fan-rows' });

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span7' }, [
			E('h3', {}, [ _('Sampling and safety') ]), fields
		]));

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span5' }, [
			E('h3', {}, [ _('Effective values') ]),
			E('p', { 'class': 'h5fan-panel-sub' }, [
				_('What the controller is using right now, after validation and clamping.')
			]),
			this.systemRows,
			E('div', { 'class': 'h5fan-actions' }, [
				E('button', {
					'class': 'btn cbi-button-negative', type: 'button',
					click: L.bind(this.confirmFactoryDefaults, this)
				}, [ _('Restore all defaults') ])
			])
		]));
	},

	buildDiagnostics: function() {
		var pane = this.panes.diagnostics;

		this.diagRows = E('div', { 'class': 'h5fan-rows' });
		this.protectRows = E('div', { 'class': 'h5fan-rows' });
		this.issueBox = E('div', {}, [ E('p', { 'class': 'h5fan-empty' }, [ _('Loading…') ]) ]);
		this.eventTable = E('table', { 'class': 'h5fan-table' }, [
			E('tr', {}, [
				E('th', {}, [ _('Time') ]), E('th', {}, [ _('Level') ]),
				E('th', {}, [ _('Code') ]), E('th', {}, [ _('Message') ])
			])
		]);

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span5' }, [
			E('h3', {}, [ _('Controller') ]), this.diagRows
		]));

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span7' }, [
			E('h3', {}, [ _('Kernel protection') ]),
			E('p', { 'class': 'h5fan-panel-sub' }, [
				_('Read-only view of the thermal zone, its governor and the trip points the firmware declares.')
			]),
			this.protectRows
		]));

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span5' }, [
			E('h3', {}, [ _('Configuration check') ]),
			E('p', { 'class': 'h5fan-panel-sub' }, [
				_('Corrections the controller applied while reading the configuration file.')
			]),
			this.issueBox
		]));

		pane.appendChild(E('div', { 'class': 'h5fan-panel h5fan-span7' }, [
			E('h3', {}, [ _('Recent events') ]),
			E('div', { 'class': 'h5fan-tablewrap h5fan-events-wrap' }, [ this.eventTable ])
		]));
	},

	/* -------------------------------------------------------------- render */

	render: function() {
		var root = E('div', { 'class': 'h5fan' });

		this.editorPoints = [];

		root.appendChild(this.styleNode());
		root.appendChild(this.head());
		root.appendChild(this.banner());
		root.appendChild(this.tabBar());
		this.buildPanes().forEach(function(pane) {
			root.appendChild(pane);
		});

		this.tabId = this.restoreTab();
		this.selectTab(this.tabId, false);
		this.markDirty();
		this.paint();

		window.addEventListener('hashchange', L.bind(function() {
			this.selectTab(this.restoreTab(), false);
		}, this));

		poll.add(L.bind(function() {
			return this.refresh().then(L.bind(function() {
				/* Keep unsaved edits visible: only refresh non-dirty fields. */
				this.restoreWidgetValues();
			}, this));
		}, this), REFRESH_SECONDS);

		return root;
	},

	restoreWidgetValues: function() {
		var key;

		for (key in OPTIONS) {
			if (this.values[key] === this.original[key] && this.widgetInput[key]) {
				if (OPTIONS[key].kind === 'switch')
					this.widgetInput[key].checked = this.values[key] === '1';
				else
					this.widgetInput[key].value = this.values[key];
			}
		}
	},

	refresh: function() {
		return this.fetchStatus().then(L.bind(function() {
			this.paint();
		}, this));
	},

	/* --------------------------------------------------------------- paint */

	paint: function() {
		if (!this.status) {
			this.paintError();
			return;
		}

		this.paintHead();
		this.paintLiveState();
		this.paintOverview();
		this.paintMonitor();
		this.paintSystem();
		this.paintDiagnostics();
		this.syncCurvePreview();
		this.renderPresets();
		this.paintError();
	},

	paintHead: function() {
		var controller = this.controller(), tone = healthTone(controller.health);

		this.headHealth.textContent = healthName(controller.health);
		this.headHealth.className = 'h5fan-health%s'.format(tone === 'ok' ? '' : ' is-%s'.format(tone));
	},

	paintError: function() {
		if (!this.bannerNode)
			return;

		clear(this.bannerNode);

		if (!this.fetchError) {
			this.bannerNode.className = 'h5fan-banner';
			return;
		}

		this.bannerNode.className = 'h5fan-banner is-shown';
		this.bannerNode.appendChild(E('strong', {}, [ _('Reading the controller failed') + ': ' + this.fetchError ]));
		this.bannerNode.appendChild(E('div', {}, [ this.status
			? _('Showing the last successful reading.')
			: _('No data available yet.') ]));
	},

	paintLiveState: function() {
		var fan = this.fanState(), thermal = this.thermal(), controller = this.controller();
		var rows = [], applied = known(fan.applied_pwm) ? fan.applied_pwm : null;
		var live = known(fan.pwm_value) ? fan.pwm_value : applied;

		rows.push([ _('Control reason'), textValue(reasonName(fan.reason)) ]);
		rows.push([ _('Mode'), textValue(this.modeName(controller.settings ? controller.settings.mode : this.settings().mode)) ]);
		rows.push([ _('Requested output'), textValue(known(fan.requested_pwm) ? '%d/255 · %d%%'.format(fan.requested_pwm, percentOfPwm(fan.requested_pwm)) : '\u2014') ]);
		rows.push([ _('Target output'), textValue(known(fan.target_pwm) ? '%d/255 · %d%%'.format(fan.target_pwm, percentOfPwm(fan.target_pwm)) : '\u2014') ]);
		rows.push([ _('Applied by controller'), textValue(known(applied) ? '%d/255 · %d%%'.format(applied, percentOfPwm(applied)) : '\u2014') ]);
		rows.push([ _('Register readback'), textValue(known(live) ? '%d/255 · %d%%'.format(live, percentOfPwm(live)) : '\u2014') ]);
		rows.push([ _('Kernel safety floor'), textValue(known(thermal.floor_pwm) ? '%d/255'.format(thermal.floor_pwm) : '\u2014',
			thermal.floor_pwm > 0 ? 'warn' : null) ]);
		rows.push([ _('Fan ownership'), textValue(thermal.owner || 'unknown') ]);
		rows.push([ _('Spin-down hold'), textValue(known(fan.down_delay_remaining_s) && fan.down_delay_remaining_s > 0
			? _('%s s remaining').format(fan.down_delay_remaining_s) : _('not holding')) ]);
		rows.push([ _('Startup boost'), textValue(fan.boost_applied ? _('used in the last cycle') : _('not used')) ]);

		if (this.controlRows)
			this.fillRows(this.controlRows, rows);

		if (this.charts) {
			this.charts.fan.h5update(this.status);
			this.charts.output.h5update(known(live) ? percentOfPwm(live) : null,
				fan.kernel_floor_applied ? 'warn' : (known(live) ? 'ok' : 'idle'));
			this.charts.target.h5update(known(fan.target_percent) ? fan.target_percent : null,
				fan.kernel_floor_applied ? 'warn' : (known(fan.target_percent) ? 'ok' : 'idle'));
		}

		if (this.stats) {
			this.stats.requested.set(known(fan.requested_pwm) ? '%d%%'.format(percentOfPwm(fan.requested_pwm)) : '\u2014',
				known(fan.requested_pwm) ? '%d/255'.format(fan.requested_pwm) : _('Unavailable'));
			this.stats.actual.set(known(live) ? '%d%%'.format(percentOfPwm(live)) : '\u2014',
				known(live) ? _('read back from %s').format(fan.pwm_path || 'pwm1') : _('Unavailable'));
			this.stats.floor.set(known(thermal.floor_pwm) ? '%d%%'.format(percentOfPwm(thermal.floor_pwm)) : '\u2014',
				thermal.basis || '', thermal.floor_pwm > 0 ? 'warn' : null);
			this.stats.reason.set(reasonName(fan.reason), controller.result ? _('result: %s').format(controller.result) : '');
			this.stats.control.set(known(this.status.control.temp_c) ? formatTemp(this.status.control.temp_c) : '\u2014',
				this.status.control.sensor_label || _('No valid sensor'),
				known(this.status.control.temp_c) ? null : 'warn');
		}
	},

	paintOverview: function() {
		var settings = this.settings(), controller = this.controller(), rows = [], badges = [];

		if (!this.overviewBadges)
			return;

		badges.push({ text: _('Mode: %s').format(this.modeName(settings.mode)) });

		if (settings.mode === 'auto')
			badges.push({ text: _('Curve: %s').format(this.profileName(settings.curve)) });

		if (settings.kernel_only)
			badges.push({ text: _('Kernel drives the fan'), tone: 'warn' });

		if (settings.curve_valid === false)
			badges.push({ text: _('Curve invalid'), tone: 'fail' });

		if (controller.sensor_failure)
			badges.push({ text: _('No valid sensor'), tone: 'fail' });

		if (this.status.sample_mode === 'cached')
			badges.push({ text: _('cached snapshot') });

		clear(this.overviewBadges);

		badges.forEach(function(badge) {
			this.overviewBadges.appendChild(E('span', {
				'class': 'h5fan-badge%s'.format(badge.tone ? ' is-%s'.format(badge.tone) : '')
			}, [ badge.text ]));
		}, this);

		rows.push([ _('Snapshot age'), textValue(known(controller.state_age_s) ? formatAge(controller.state_age_s) : _('Never'),
			controller.stale ? 'warn' : 'ok') ]);
		rows.push([ _('Running'), textValue(controller.running ? _('yes (PID %s)').format(controller.pid) : _('no'),
			controller.running ? 'ok' : 'warn') ]);
		rows.push([ _('Consecutive failures'), textValue(String(controller.consecutive_failures || 0),
			controller.consecutive_failures ? 'fail' : null) ]);
		rows.push([ _('Last error'), textValue(dash(controller.last_error), controller.last_error ? 'fail' : null) ]);

		this.fillRows(this.overviewRows, rows);
	},

	paintMonitor: function() {
		var sensors = (this.status && this.status.sensors) || [];

		(this.allBars || []).forEach(L.bind(function(bar) {
			var reading = null, i;

			for (i = 0; i < sensors.length; i++) {
				if (sensors[i].id === bar.id)
					reading = sensors[i];
			}

			bar.chart.h5update(reading);
		}, this));

		if (!this.monitorTable)
			return;

		while (this.monitorTable.children.length > 1)
			this.monitorTable.removeChild(this.monitorTable.lastChild);

		sensors.forEach(function(reading) {
			var tone = reading.valid ? 'ok' : (reading.status === 'missing' ? null : 'warn');

			this.monitorTable.appendChild(E('tr', {}, [
				E('td', {}, [ _(reading.title || reading.id) ]),
				E('td', {}, [ reading.valid ? formatTemp(reading.value_c) : _('Unknown') ]),
				E('td', {}, [ tag(_(stateName(reading.status)), tone) ]),
				E('td', {}, [ dash(reading.source) ]),
				E('td', {}, [ formatAge(reading.age_s) ]),
				E('td', {}, [ reading.error || dash(reading.label) ])
			]));
		}, this);
	},

	paintSystem: function() {
		var settings = this.settings(), rows = [], fan = this.fanState();

		if (!this.systemRows)
			return;

		[ 'interval', 'hysteresis', 'down_delay', 'start_pwm', 'start_boost_ms', 'fail_pwm' ].forEach(L.bind(function(key) {
			var spec = OPTIONS[key];
			this.validateOption(key, false);
			rows.push([ _('Saved %s').format(_(spec.label)), textValue('%s %s'.format(dash(settings[key]), spec.unit || '')) ]);
		}, this));

		rows.push([ _('Control temperature source'), textValue(settings.temp_source === 'cpu' ? _('CPU only') : _('Hottest valid sensor')) ]);
		rows.push([ _('Effective curve'), textValue('%s · %s'.format(this.profileName(settings.curve), dash(settings.curve_data))) ]);
		rows.push([ _('Schema version'), textValue(dash(settings.schema_version)) ]);
		rows.push([ _('Controller version'), textValue(dash(this.controller().version)) ]);

		this.fillRows(this.systemRows, rows);
	},

	paintDiagnostics: function() {
		var controller = this.controller(), thermal = this.thermal(), protections = this.protections();
		var paths = this.status.paths || {}, rows = [];

		if (!this.diagRows)
			return;

		rows.push([ _('Health'), textValue(healthName(controller.health), healthTone(controller.health)) ]);
		rows.push([ _('Result'), textValue(controller.result || 'none') ]);
		rows.push([ _('Cycles'), textValue(String(controller.cycle_count || 0)) ]);
		rows.push([ _('Last good cycle'), textValue(controller.last_good_cycle ? formatClock(controller.last_good_cycle) : _('Never')) ]);
		rows.push([ _('Sampling interval'), textValue('%s s'.format(dash(controller.interval_s))) ]);
		rows.push([ _('Status source'), textValue(this.status.sample_mode === 'live' ? _('live sample') : _('published snapshot')) ]);
		rows.push([ _('Legacy policy record'), textValue(thermal.legacy_policy_pending
			? _('waiting to be restored') : _('none'), thermal.legacy_policy_pending ? 'warn' : null) ]);

		this.fillRows(this.diagRows, rows);

		this.fillRows(this.protectRows, [
			[ _('Kernel protections'), textValue(protections.intact ? _('intact') : _('incomplete'),
				protections.intact ? 'ok' : 'fail') ],
			[ _('Thermal governor'), textValue(dash(protections.policy)) ],
			[ _('Governor hijacked'), textValue(protections.governor_hijacked ? _('user_space in use') : _('no'),
				protections.governor_hijacked ? 'fail' : 'ok') ],
			[ _('CPU throttling device'), textValue(protections.cpufreq_cooling ? _('registered') : _('missing'),
				protections.cpufreq_cooling ? 'ok' : 'warn') ],
			[ _('Fan cooling device'), textValue(dash(protections.fan_cooling_device)) ],
			[ _('Critical trip'), textValue(this.criticalTripText(protections)) ],
			[ _('Cooling levels'), textValue((thermal.cooling_levels || []).join(' / ') || '\u2014') ],
			[ _('Floor basis'), textValue(dash(thermal.basis)) ]
		]);

		if (protections.warnings && protections.warnings.length) {
			protections.warnings.forEach(L.bind(function(warning) {
				this.protectRows.appendChild(E('p', { 'class': 'h5fan-note is-warn' }, [ warning ]));
			}, this));
		}

		clear(this.issueBox);

		if (!(this.settings().issues || []).length)
			this.issueBox.appendChild(E('p', { 'class': 'h5fan-empty' }, [ _('The configuration was accepted as-is.') ]));
		else
			(this.settings().issues || []).forEach(function(issue) {
				this.issueBox.appendChild(E('p', { 'class': 'h5fan-note is-warn' }, [
					_('%1$s = %2$s: %3$s (using %4$s)').format(issue.option, dash(issue.value), issue.problem, issue.applied)
				]));
			}, this);

		while (this.eventTable.children.length > 1)
			this.eventTable.removeChild(this.eventTable.lastChild);

		if (!(this.status.events || []).length) {
			this.eventTable.appendChild(E('tr', {}, [
				E('td', { colspan: '4', 'class': 'h5fan-empty' }, [ _('No events recorded yet') ])
			]));
		}

		(this.status.events || []).slice(0, 40).forEach(function(event) {
			this.eventTable.appendChild(E('tr', {}, [
				E('td', {}, [ formatClock(event.ts) ]),
				E('td', {}, [ tag(event.level, event.level === 'error' ? 'fail' : (event.level === 'warn' ? 'warn' : 'ok')) ]),
				E('td', {}, [ dash(event.code) ]),
				E('td', {}, [ dash(event.message) ])
			]));
		}, this);

		[
			[ _('Configuration file'), paths.uci_config ],
			[ _('State file'), paths.state ],
			[ _('Event journal'), paths.event_log ],
			[ _('Lock directory'), paths.lock_dir ]
		].forEach(L.bind(function(pair) {
			this.diagRows.appendChild(row(pair[0], textValue(dash(pair[1]))));
		}, this));
	},

	criticalTripText: function(protections) {
		var trips = protections.trips || [], i;

		for (i = 0; i < trips.length; i++) {
			if (trips[i].type === 'critical' && known(trips[i].temp_c))
				return formatTemp(trips[i].temp_c);
		}

		return _('not declared');
	},

	fillRows: function(node, entries) {
		clear(node);

		entries.forEach(function(entry) {
			node.appendChild(row(entry[0], entry[1]));
		});
	},

	modeName: function(mode) {
		if (mode === 'manual')
			return _('Manual');

		if (mode === 'kernel')
			return _('Kernel protection only');

		return _('Automatic');
	},

	profileName: function(profile) {
		if (profile === 'silent')
			return _('Quiet');

		if (profile === 'performance')
			return _('Performance');

		if (profile === 'custom')
			return _('Custom');

		return _('Balanced');
	},

	/* ------------------------------------------------------- curve editor */

	renderPresets: function() {
		var seg = document.getElementById('h5fan-preset-seg');

		if (!seg)
			return;

		clear(seg);

		(OPTIONS.curve.choices || []).forEach(L.bind(function(choice) {
			var input = E('input', {
				type: 'radio', name: 'h5fan-preset', value: choice[0]
			});

			input.checked = this.values.curve === choice[0];
			input.addEventListener('change', L.bind(function() {
				this.values.curve = choice[0];
				this.syncWidget('curve');
				this.afterChange('curve');
			}, this));

			seg.appendChild(E('label', { 'class': this.values.curve === choice[0] ? 'is-on' : null }, [
				input, _(choice[1])
			]));
		}, this));
	},

	syncWidget: function(key) {
		var input = this.widgetInput[key];

		if (!input)
			return;

		if (OPTIONS[key].kind === 'switch')
			input.checked = this.values[key] === '1';
		else
			input.value = this.values[key];

		if (this.widgetRange && this.widgetRange[key])
			this.widgetRange[key].value = this.values[key];
	},

	editorFromSpec: function(spec) {
		this.editorPoints = parseCurve(spec).points.map(function(point) {
			return { temp: point.temp, percent: point.percent };
		});

		if (!this.editorPoints.length) {
			this.editorPoints = OPTIONS.curve_custom.def.split(',').map(function(pair) {
				var parts = pair.split(':');
				return { temp: parseInt(parts[0], 10), percent: parseInt(parts[1], 10) };
			});
		}

		this.renderNodes();
	},

	renderNodes: function() {
		if (!this.nodeList)
			return;

		clear(this.nodeList);

		this.editorPoints.forEach(L.bind(function(point, index) {
			var temp = E('input', {
				type: 'number', 'class': 'cbi-input-text', min: '0', max: '150', step: '1', value: String(point.temp)
			});
			var percent = E('input', {
				type: 'number', 'class': 'cbi-input-text', min: '0', max: '100', step: '1', value: String(point.percent)
			});

			temp.addEventListener('input', L.bind(function(ev) {
				this.editorPoints[index].temp = num(ev.target.value);
				this.commitNodes();
			}, this));
			percent.addEventListener('input', L.bind(function(ev) {
				this.editorPoints[index].percent = num(ev.target.value);
				this.commitNodes();
			}, this));

			this.nodeList.appendChild(E('div', { 'class': 'h5fan-noderow' }, [
				E('label', {}, [ _('Temperature °C'), temp ]),
				E('label', {}, [ _('Output %'), percent ]),
				E('button', {
					'class': 'btn cbi-button-negative', type: 'button',
					'aria-label': _('Delete point'),
					click: L.bind(function() {
						this.editorPoints.splice(index, 1);
						this.commitNodes();
						this.renderNodes();
					}, this)
				}, [ '\u00d7' ])
			]));
		}, this));

		this.commitNodes();
	},

	commitNodes: function() {
		var sorted = this.editorPoints.filter(function(point) {
			return known(point.temp) && known(point.percent);
		}).sort(function(a, b) {
			return a.temp - b.temp;
		});
		var spec = sorted.map(function(point) {
			return '%d:%d'.format(point.temp, point.percent);
		}).join(',');
		var parsed = parseCurve(spec);

		this.nodeError.textContent = parsed.error || '';

		if (!parsed.error || spec === '') {
			this.values.curve_custom = spec;
			this.syncWidget('curve_custom');
			this.validateOption('curve_custom', true);
			this.markDirty();
		}

		this.syncCurvePreview();
	},

	syncCurvePreview: function() {
		var settings = this.settings(), fan = this.fanState(), key, spec;

		if (!this.charts)
			return;

		/* Draw the curve that would be effective with the draft values, using
		 * the same profile-to-option mapping the controller applies. */
		key = CURVE_OPTION[this.values.curve] || CURVE_OPTION.balanced;
		spec = known(this.values[key]) && this.values[key] !== ''
			? this.values[key]
			: (settings[key] || settings.curve_data || OPTIONS[key].def);

		this.drawCurve(spec);
	},

	drawCurve: function(spec) {
		var points = parseCurve(spec), fan = this.fanState();

		this.charts.curve.h5update({
			points: points.points,
			error: points.error,
			control: this.status ? this.status.control : null,
			targetPercent: known(fan.target_percent) ? fan.target_percent : null,
			floorPercent: known(this.thermal().floor_percent) ? this.thermal().floor_percent : null
		});
	},

	/* ---------------------------------------------------------- save paths */

	confirmRestart: function() {
		return this.confirm(_('Restart the fan control service? The fan keeps its current output while the service restarts.'))
			.then(L.bind(function(ok) {
				if (!ok)
					return null;

				return fs.exec(INITD_CMD, [ 'restart' ]).then(L.bind(function(result) {
					if (result && result.code !== 0)
						this.notify(_('The service could not be restarted: %s').format(result.stderr || ''), 'danger');
					else
						this.notify(_('Service restarted.'), 'info');

					return this.refresh();
				}, this));
			}, this)).catch(L.bind(function(err) {
				this.notify(_('The service could not be restarted: %s').format(err.message || err), 'danger');
			}, this));
	},

	confirmFactoryDefaults: function() {
		return this.confirm(_('Restore every option to the values shipped with the package? Nothing is written until you save.'))
			.then(L.bind(function(ok) {
				if (!ok)
					return null;

				Object.keys(OPTIONS).forEach(L.bind(function(key) {
					this.values[key] = OPTIONS[key].def;
				}, this));

				this.rebuildWidgets();
				this.editorFromSpec(OPTIONS.curve_custom.def);
				this.markDirty();
				this.notify(_('Defaults loaded into the form. Save to store them.'), 'info');
				return null;
			}, this));
	},

	resetCustomCurve: function() {
		this.editorFromSpec(OPTIONS.curve_custom.def);
		this.notify(_('The default custom curve was loaded into the editor. Save to store it.'), 'info');
	},

	rebuildWidgets: function() {
		Object.keys(OPTIONS).forEach(L.bind(function(key) {
			this.syncWidget(key);
		}, this));

		this.renderPresets();
		this.syncCurvePreview();
	},

	confirm: function(message) {
		var messages = Array.prototype.slice.call(arguments).filter(Boolean);

		if (!messages.length)
			return Promise.resolve(true);

		/*
		 * Built on ui.showModal rather than ui.confirm: this is the dialog the
		 * released page already uses, so the button wiring is known to work on
		 * the LuCI build the H5000M ships.
		 */
		return new Promise(function(resolve) {
			ui.showModal(_('Please confirm'), [
				E('div', { 'style': 'margin-bottom:12px;line-height:1.6' },
					messages.map(function(text) {
						return E('p', {}, [ text ]);
					})),
				E('div', { 'class': 'right' }, [
					E('button', {
						'class': 'btn',
						click: function() {
							ui.hideModal();
							resolve(false);
						}
					}, [ _('Cancel') ]),
					' ',
					E('button', {
						'class': 'btn cbi-button-positive important',
						click: function() {
							ui.hideModal();
							resolve(true);
						}
					}, [ _('Continue') ])
				])
			]);
		});
	},

	notify: function(messages, type) {
		var list = (Array.isArray(messages) ? messages : [ messages ]).filter(Boolean);

		ui.addNotification(null, E('div', {}, list.map(function(text) {
			return E('p', {}, [ text ]);
		})), type || 'info');
	},

	handleSave: function(ev) {
		return this.commit(false, ev);
	},

	handleSaveApply: function(ev) {
		return this.commit(true, ev);
	},

	commit: function(applyNow, ev) {
		var self = this, problems = this.validateAll(), dirty = this.dirtyKeys(), risky = this.riskyWarnings();

		if (ev)
			ev.preventDefault();

		if (problems.length) {
			this.notify([ _('Nothing was saved. Please correct:') ].concat(problems), 'danger');
			return Promise.resolve();
		}

		var proceed = risky.length ? this.confirm(risky.join('\n')) : Promise.resolve(true);

		return proceed.then(function(confirmed) {
			if (!confirmed) {
				self.notify(_('Saving cancelled; your input is still in the form.'), 'warning');
				return null;
			}

			if (!dirty.length) {
				if (!applyNow) {
					self.notify(_('No changes to save.'), 'info');
					return null;
				}

				return self.runApply().then(function() {
					self.notify(_('Configuration unchanged; one control cycle ran.'), 'info');
				});
			}

			return self.writeUci(applyNow, dirty);
		});
	},

	riskyWarnings: function() {
		var warnings = [];

		if (this.original.enabled === '1' && this.values.enabled === '0')
			warnings.push(_('The enhanced controller will be disabled and the fan will follow the kernel thermal policy only. Continue?'));

		if (this.values.mode === 'manual' && isUInt(this.values.manual_pwm)) {
			var value = parseInt(this.values.manual_pwm, 10);

			if (value === 0)
				warnings.push(_('Manual output 0 commands the fan to stop. Temperatures will rise; the kernel floor may still raise the output. Continue?'));
			else if (value < 80)
				warnings.push(_('Manual output below 80/255 often cannot keep the fan spinning under load. Continue?'));
		}

		if (this.values.mode === 'auto' && this.values.curve === 'custom' && parseCurve(this.values.curve_custom).error)
			warnings.push(_('The custom curve is not usable, so the controller would run its failsafe output continuously. Continue?'));

		return warnings;
	},

	policyChanged: function(dirty) {
		return dirty.some(function(key) {
			return POLICY_KEYS.indexOf(key) > -1;
		});
	},

	writeUci: function(applyNow, dirty) {
		var self = this, key;

		if (!uci.get(CFG, SID))
			uci.add(CFG, 'settings', SID);

		for (key in OPTIONS)
			uci.set(CFG, SID, key, String(this.values[key]));

		return uci.save().then(function() {
			return uci.apply();
		}).then(function() {
			for (key in OPTIONS)
				self.original[key] = String(self.values[key]);

			self.markDirty();

			if (!applyNow) {
				self.notify(_('Settings saved. The controller picks them up within one control interval.'), 'info');
				return self.refresh();
			}

			if (!self.policyChanged(dirty)) {
				self.notify(_('Settings saved; no control-relevant value changed.'), 'info');
				return self.refresh();
			}

			return self.runApply().then(function() {
				self.notify(_('Settings saved and applied.'), 'info');
				return self.refresh();
			});
		}).catch(function(err) {
			self.notify(_('Saving failed: %s').format((err && err.message) ? err.message : err), 'danger');
			throw err;
		});
	},

	handleReset: function() {
		var self = this;

		return uci.unload(CFG).then(function() {
			return uci.load(CFG);
		}).then(function() {
			self.readStoredConfig();
			self.rebuildWidgets();
			self.editorFromSpec(self.values.curve_custom);
			self.markDirty();
			self.notify(_('Unsaved changes were discarded.'), 'info');
			self.paint();
		}, function(err) {
			self.notify(_('The configuration could not be reloaded: %s').format((err && err.message) ? err.message : err), 'danger');
		});
	},

	runApply: function() {
		var self = this;

		return fs.exec(APPLY_CMD, [ 'apply' ]).then(function(result) {
			if (result && result.code !== 0)
				throw new Error(String(result.stderr || '').trim() || _('the controller reported a failure'));

			return result;
		}).catch(function(err) {
			self.notify(_('Applying failed: %s').format(err.message || err), 'danger');
			throw err;
		});
	}
});
