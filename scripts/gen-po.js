#!/usr/bin/env node
/*
 * Generate po/zh_Hans/h5000m-fancontrol.po from the strings the LuCI view uses.
 *
 * Maintainer tool: run `node scripts/gen-po.js` after changing any user visible
 * text. It refuses to write anything unless every extracted string has a
 * translation here, so the catalogue can never drift behind the view.
 */

'use strict';

const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

const repo = path.join(__dirname, '..');
const PO = path.join(repo, 'po', 'zh_Hans', 'h5000m-fancontrol.po');

const zh = {
	'%1$s = %2$s: %3$s (using %4$s)': '%1$s = %2$s：%3$s（已改用 %4$s）',
	'%1$s at %2$s gives %3$d%% output': '%1$s 在 %2$s 时输出 %3$d%%',
	'%1$s must be a whole number between %2$s and %3$s.': '%1$s 必须是 %2$s 到 %3$s 之间的整数。',
	'%s is not a pair of whole numbers.': '%s 不是一对整数。',
	'%s s remaining': '还需 %s 秒',
	'%s °C': '%s °C',
	'%s · raised by the kernel floor': '%s · 已被内核下限抬高',
	'0 is stopped, 255 is full speed.': '0 为停转，255 为全速。',
	'5G modem': '5G 模组',
	'Add point': '添加节点',
	'Allowed': '取值范围',
	'Applied by controller': '控制器已输出',
	'Apply now': '立即应用',
	'Apply runs one control cycle immediately. The controller reloads its configuration by itself, so saving never restarts the service.':
		'「立即应用」会马上执行一个控制周期。控制器自身会重新读取配置，因此保存不会重启服务。',
	'Applying failed: %s': '应用失败：%s',
	Automatic: '自动',
	'Automatic curve': '自动曲线',
	'Automatic uses the fan curve, Manual keeps a fixed output, Kernel protection only leaves the fan to the kernel.':
		'自动模式使用风扇曲线；手动模式保持固定输出；仅内核保护则把风扇完全交给内核。',
	Balanced: '均衡',
	'Balanced curve': '均衡曲线',
	'Balances cooling against noise without touching the kernel CPU throttling, hot or critical protection.':
		'在散热与噪音之间取得平衡，同时不影响内核的 CPU 降频、高温及 critical 过热保护。',
	'Blue dashed: target output': '蓝色虚线：目标输出',
	CPU: 'CPU',
	'CPU only': '仅 CPU',
	'CPU throttling device': 'CPU 降频设备',
	Cancel: '取消',
	'Choose enabled or disabled.': '请选择启用或禁用。',
	Code: '事件码',
	'Comma separated temperature:percent pairs, temperatures strictly increasing.':
		'以逗号分隔的「温度:百分比」对，温度必须严格递增。',
	'Configuration check': '配置检查',
	'Configuration file': '配置文件',
	'Configuration unchanged; one control cycle ran.': '配置未变化；已执行一个控制周期。',
	'Consecutive failures': '连续失败次数',
	Continue: '继续',
	'Control reason': '控制归因',
	'Control temperature': '控制温度',
	'Control temperature source': '控制温度来源',
	Controller: '控制器',
	'Controller error': '控制器异常',
	'Controller not running': '控制器未运行',
	'Controller version': '控制器版本',
	'Cooling levels': '散热档位',
	'Cooling profile': '风扇曲线',
	'Corrections the controller applied while reading the configuration file.':
		'控制器读取配置文件时所做的修正。',
	'Critical trip': 'critical 触发点',
	'Curve failsafe': '曲线失效保护',
	'Curve invalid': '曲线无效',
	'Curve unavailable: %s': '曲线不可用：%s',
	'Curve: %s': '曲线：%s',
	Custom: '自定义',
	'Custom curve': '自定义曲线',
	'Custom curve points': '自定义曲线节点',
	Cycles: '控制周期数',
	'Defaults loaded into the form. Save to store them.': '已把出厂默认值填入表单，保存后才会写入。',
	'Delete point': '删除该节点',
	Detail: '详细说明',
	Diagnostics: '诊断与日志',
	'Editing here only changes the page. Use Save to store it, and the chart updates live.':
		'在此编辑只影响页面，保存后才会写入；曲线图会实时更新。',
	'Effective curve': '当前生效曲线',
	'Effective values': '生效值',
	'Enable enhanced controller': '启用增强控制器',
	Enabled: '已启用',
	'Ethernet PHY': '以太网 PHY',
	'Event journal': '事件日志',
	'Every point on the line is the output the controller applies at that temperature.':
		'线上每一点都是控制器在该温度下实际施加的输出。',
	'Extra degrees applied when slowing down, so the fan does not oscillate around a curve point.':
		'降速时额外叠加的温度，避免风扇在曲线拐点附近反复波动。',
	'Failsafe cooling': '失效保护散热',
	'Failsafe cooling active': '失效保护散热中',
	'Failsafe output': '失效保护输出',
	Fan: '风扇',
	'Fan Control': '风扇控制',
	'Fan control': '风扇控制',
	'Fan control node not found': '未找到风扇控制节点',
	'Fan cooling device': '风扇散热设备',
	'Fan curve': '温控曲线',
	'Fan diagram': '风扇示意图',
	'Fan output %1$s, %2$s': '风扇输出 %1$s，%2$s',
	'Fan ownership': '风扇控制权',
	'Floor basis': '下限依据',
	'Governor hijacked': 'governor 被接管',
	'H5000M cooling management': 'H5000M 散热管理',
	Health: '健康状态',
	'Hold the current output this long before slowing down. Speed-ups stay immediate.':
		'降速前先维持当前输出这么长时间；提速始终立即生效。',
	'Holding output before slowing down': '降速前维持输出',
	'Hottest valid sensor': '最高有效传感器',
	'Just now': '刚刚',
	'Kernel drives the fan': '由内核驱动风扇',
	'Kernel floor': '内核下限',
	'Kernel protection': '内核保护',
	'Kernel protection only': '仅内核保护',
	'Kernel protections': '内核保护',
	'Kernel safety floor': '内核安全下限',
	'Last %s s ago': '%s 秒前',
	'Last error': '最近错误',
	'Last good cycle': '最近成功周期',
	'Legacy policy record': '历史策略记录',
	Level: '级别',
	'Live output': '实时输出',
	'Loading…': '加载中…',
	'Lock directory': '锁目录',
	Manual: '手动',
	'Manual PWM output': '手动 PWM 输出',
	'Manual output 0 commands the fan to stop. Temperatures will rise; the kernel floor may still raise the output. Continue?':
		'手动输出 0 会让风扇停转，温度将上升；内核安全下限仍可能抬高输出。确定继续？',
	'Manual output below 80/255 often cannot keep the fan spinning under load. Continue?':
		'手动输出低于 80/255 时，带载下风扇常常无法维持转动。确定继续？',
	'Manual request': '手动请求',
	'Marker: current control temperature': '标记：当前控制温度',
	Message: '内容',
	Mode: '模式',
	'Mode: %s': '模式：%s',
	'Native thermal and hwmon readings win; the vendor radio query is only a bounded fallback. The colour bands are this project’s own reference ranges, not a vendor specification.':
		'原生 thermal 与 hwmon 读数优先；厂商射频查询只是有界回退。颜色分段是本项目的参考区间，不是厂商规格。',
	Never: '从未',
	'Never sampled': '尚无采样',
	'No changes to save.': '没有需要保存的更改。',
	'No cycle published yet': '尚未发布控制周期',
	'No data available yet.': '暂时还没有数据。',
	'No events recorded yet': '暂无事件记录',
	'No feedback': '无转速反馈',
	'No output reading': '无输出读数',
	'No valid sensor': '无有效传感器',
	'Nothing was saved. Please correct:': '未保存任何更改，请先修正：',
	'Only used in automatic mode.': '仅在自动模式下使用。',
	'Operating mode': '运行模式',
	'Output %': '输出百分比',
	'Output %d%%': '输出 %d%%',
	Outputs: '输出',
	Overview: '总览',
	'Percentage %s is above 100%.': '百分比 %s 超过 100%。',
	Performance: '性能',
	'Performance curve': '性能曲线',
	'Pick one of the listed values.': '请从候选值中选择。',
	'Please confirm': '请确认',
	'Preset used by the Balanced profile.': '均衡档所使用的预设曲线。',
	'Preset used by the Performance profile.': '性能档所使用的预设曲线。',
	'Preset used by the Quiet profile. Restore defaults puts this line back.':
		'静音档所使用的预设曲线，恢复默认值会还原此行。',
	Quiet: '静音',
	'Quiet curve': '静音曲线',
	'Raised by the kernel floor': '已被内核下限抬高',
	'Read-only view of the thermal zone, its governor and the trip points the firmware declares.':
		'只读展示散热区、其 governor 以及固件声明的触发点。',
	'Reading the controller failed': '读取控制器失败',
	'Recent events': '最近事件',
	'Red dashed: kernel safety floor': '红色虚线：内核安全下限',
	Refresh: '刷新',
	'Register readback': '寄存器回读',
	'Remove last': '删除末尾节点',
	'Reported by the tachometer': '来自测速风扇脚',
	Requested: '请求输出',
	'Requested output': '请求输出',
	'Restart service': '重启服务',
	'Restart the fan control service? The fan keeps its current output while the service restarts.':
		'要重启风扇控制服务吗？重启期间风扇保持当前输出。',
	'Restore all defaults': '恢复全部默认值',
	'Restore default curve': '还原默认曲线',
	'Restore every option to the values shipped with the package? Nothing is written until you save.':
		'把所有选项恢复为软件包出厂值？保存之前不会写入任何内容。',
	Result: '结果',
	Revert: '撤销更改',
	Running: '运行中',
	'Running normally': '运行正常',
	'Sampling and safety': '采样与安全',
	'Sampling interval': '采样间隔',
	Save: '保存',
	'Save & Apply': '保存并应用',
	'Saved %s': '已保存%s',
	'Saving cancelled; your input is still in the form.': '已取消保存；你的输入仍保留在表单中。',
	'Saving failed: %s': '保存失败：%s',
	'Schema version': '配置版本',
	'Schematic fan. The animation follows the applied controller output and is not a measured shaft speed.':
		'风扇示意图。动画跟随控制器输出的实际值，不是实测转速。',
	'Schematic only. The animation follows the applied output, it is not a measured speed.':
		'仅为示意。动画跟随已输出的百分比，不是实测转速。',
	'Seconds between control decisions. The page refreshes independently of this.':
		'两次控制决策之间的秒数。页面刷新节奏与此独立。',
	Sections: '分区',
	Sensor: '传感器',
	'Sensor failsafe': '传感器失效保护',
	'Service restarted.': '服务已重启。',
	'Set 0 to disable the boost.': '设为 0 即关闭启动助推。',
	Settings: '系统设置',
	'Settings saved and applied.': '设置已保存并应用。',
	'Settings saved. The controller picks them up within one control interval.':
		'设置已保存，控制器会在一个采样周期内生效。',
	'Settings saved; no control-relevant value changed.': '设置已保存；没有影响控制的取值变化。',
	'Short high pulse written only when the fan is standing still.': '仅在风扇静止时写入的短促高电平启动脉冲。',
	'Showing the last successful reading.': '当前显示的是最近一次成功读取的数据。',
	'Snapshot age': '快照时效',
	'Solid green: the control function': '绿色实线：控制函数',
	Source: '来源',
	'Spin-down delay': '降速延迟',
	'Spin-down hold': '降速保持',
	'Startup boost': '启动助推',
	'Startup boost PWM': '启动助推 PWM',
	'Startup boost duration': '启动助推时长',
	State: '状态',
	'State file': '状态文件',
	Status: '状态',
	'Status is stale': '状态已过期',
	'Status source': '状态来源',
	'Stopped by policy': '策略已停转',
	Tachometer: '转速反馈',
	Target: '目标输出',
	'Target output': '目标输出',
	Temperature: '温度监控',
	'Temperature %s °C does not rise above %s °C.': '温度 %s °C 未高于前一点 %s °C。',
	'Temperature %s °C is above the supported 150 °C.': '温度 %s °C 超出支持的 150 °C。',
	'Temperature (°C)': '温度（°C）',
	'Temperature hysteresis': '温度滞回',
	'Temperature monitor': '温度监控',
	'Temperature to fan output curve': '温度与风扇输出曲线',
	'Temperature °C': '温度 °C',
	Temperatures: '温度',
	'Temperatures must rise strictly and percentages stay within 0-100%. An invalid curve makes the controller run its failsafe output instead of stopping the fan.':
		'温度必须严格递增，百分比须保持在 0-100%。曲线无效时控制器会转入失效保护输出，而不是让风扇停转。',
	'The configuration could not be reloaded: %s': '无法重新载入配置：%s',
	'The configuration was accepted as-is.': '配置已按原样接受。',
	'The curve accepts at most 32 points.': '曲线最多支持 32 个节点。',
	'The curve needs at least two points.': '曲线至少需要两个节点。',
	'The custom curve is not usable, so the controller would run its failsafe output continuously. Continue?':
		'自定义曲线不可用，控制器会持续运行在失效保护输出。确定继续？',
	'The default custom curve was loaded into the editor. Save to store it.':
		'已在编辑器中载入默认自定义曲线，保存后才会写入。',
	'The enhanced controller will be disabled and the fan will follow the kernel thermal policy only. Continue?':
		'增强控制器将被禁用，风扇只跟随内核散热策略。确定继续？',
	'The hottest option considers CPU, Ethernet PHY, both radios and the 5G modem cache.':
		'最高温模式会考虑 CPU、以太网 PHY、两路射频以及 5G 模组缓存。',
	'The kernel safety floor can still raise the effective output above this value.':
		'内核安全下限仍可能把实际输出抬高到此值之上。',
	'The line is sampled once per degree with the same integer arithmetic the controller uses.':
		'曲线按每 1 °C 采样，使用与控制器完全一致的整数运算。',
	'The service could not be restarted: %s': '服务重启失败：%s',
	'The status command exited with code %s': '状态命令以退出码 %s 结束',
	'The status reply does not match the expected contract': '状态返回与约定的数据契约不符',
	'Thermal governor': '散热 governor',
	'This board exposes no tachometer reading': '该机型未提供转速读数',
	Time: '时间',
	Unavailable: '不可用',
	Unknown: '未知',
	'Unsaved changes were discarded.': '已放弃未保存的更改。',
	Updated: '更新时间',
	'Used when no sensor or no usable curve is available; the range keeps it above 75%.':
		'无传感器或无可用曲线时使用；取值范围保证其高于 75%。',
	Value: '数值',
	'What the controller is using right now, after validation and clamping.':
		'控制器当前实际使用的值，已完成校验与限幅。',
	'When disabled the fan follows the kernel thermal policy only; CPU throttling and over-temperature protection are unaffected.':
		'禁用时风扇只跟随内核散热策略；CPU 降频与过热保护不受影响。',
	'Wi-Fi radio 1': 'Wi-Fi 射频 1',
	'Wi-Fi radio 2': 'Wi-Fi 射频 2',
	'Write each point as temperature:percent, for example 40:50.': '每个点写成「温度:百分比」，例如 40:50。',
	'Writing the fan output failed': '写入风扇输出失败',
	'cached snapshot': '缓存快照',
	'control temperature': '控制温度',
	incomplete: '不完整',
	intact: '完好',
	'live sample': '实时采样',
	missing: '缺失',
	no: '否',
	'no reading': '无读数',
	none: '无',
	'not declared': '未声明',
	'not holding': '未在保持',
	'not used': '未使用',
	'of full speed': '占全速',
	'published snapshot': '已发布快照',
	'read back from %s': '回读自 %s',
	registered: '已注册',
	'result: %s': '结果：%s',
	'the controller reported a failure': '控制器报告失败',
	unknown: '未知',
	'used in the last cycle': '上一周期已使用',
	'user_space in use': '正处于 user_space',
	'waiting to be restored': '待恢复',
	'yes (PID %s)': '是（PID %s）',
};

function msgidsFromView() {
	const source = execFileSync(process.execPath, [path.join(__dirname, 'i18n-audit.js'), '--list'], {
		cwd: repo,
		encoding: 'utf8',
	});

	return source.split('\n').filter((line) => line.length > 0);
}

function escapePo(text) {
	return text.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n');
}

function main() {
	const ids = msgidsFromView();
	const missing = ids.filter((id) => !Object.prototype.hasOwnProperty.call(zh, id));

	if (missing.length) {
		console.error(`${missing.length} string(s) have no translation defined in scripts/gen-po.js:`);
		missing.forEach((text) => console.error(`  ${text}`));
		process.exit(1);
	}

	/* A complete header keeps msgfmt and LuCI's i18n tooling quiet. */
	const header = [
		'Project-Id-Version: luci-app-h5000m-fancontrol 3.0.0',
		'POT-Creation-Date: 2026-10-25 12:00+0800',
		'PO-Revision-Date: 2026-10-25 12:00+0800',
		'Last-Translator: \n',
		'Language-Team: Chinese (simplified)',
		'Language: zh_Hans',
		'MIME-Version: 1.0',
		'Content-Type: text/plain; charset=UTF-8',
		'Content-Transfer-Encoding: 8bit',
		'Plural-Forms: nplurals=1; plural=0;',
	];

	const out = ['msgid ""', 'msgstr ""', ...header.map((line) => `"${escapePo(line)}\\n"`), ''];

	ids.forEach((id) => {
		/* Multi-line msgid form keeps the file readable for long sentences. */
		if (id.length > 72) {
			out.push('msgid ""');
			out.push(`"${escapePo(id)}"`);
		} else {
			out.push(`msgid "${escapePo(id)}"`);
		}

		const translation = zh[id];

		if (translation.length > 72) {
			out.push('msgstr ""');
			out.push(`"${escapePo(translation)}"`);
		} else {
			out.push(`msgstr "${escapePo(translation)}"`);
		}

		out.push('');
	});

	fs.mkdirSync(path.dirname(PO), { recursive: true });
	fs.writeFileSync(PO, out.join('\n'), { encoding: 'utf8' });
	console.log(`wrote ${ids.length} entries to ${path.relative(repo, PO)}`);
}

main();
