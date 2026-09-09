<?php
// Standalone ground-truth generator for Pref_Prefs::appPasswordList (the
// app-password table). Replicates the method's HTML verbatim (clean/__ = app
// behavior). Uses a fixed in-memory dataset; make_local_datetime is a stub that
// emits a deterministic marker (the structural comparison normalizes dates out),
// since the real TimeHelper::make_local_datetime depends on session/timezone.

function __($s) { return $s; }
function make_local_datetime($ts) {
    return $ts ? "DT:" . $ts : "Never";
}

$rows = [
    ["id" => 1, "title" => "First API",  "created" => "2026-06-15 14:30:00", "last_used" => null],
    ["id" => 2, "title" => "Second API", "created" => "2026-06-15 14:30:00", "last_used" => "2026-06-16 09:00:00"],
];
?>
<div dojoType='fox.Toolbar'>
	<div dojoType='fox.form.DropDownButton'>
		<span><?= __('Select') ?></span>
		<div dojoType='dijit.Menu' style='display: none'>
			<div onclick="Tables.select('app-password-list', true)"
				dojoType="dijit.MenuItem"><?= __('All') ?></div>
			<div onclick="Tables.select('app-password-list', false)"
				dojoType="dijit.MenuItem"><?= __('None') ?></div>
		</div>
	</div>
</div>

<div class='panel panel-scrollable'>
	<table width='100%' id='app-password-list'>
		<tr>
			<th class="checkbox"> </th>
			<th width='50%'><?= __("Description") ?></th>
			<th><?= __("Created") ?></th>
			<th><?= __("Last used") ?></th>
		</tr>
		<?php foreach ($rows as $pass) { ?>
		<tr data-row-id='<?= $pass['id'] ?>'>
			<td class="checkbox">
				<input onclick='Tables.onRowChecked(this)' dojoType='dijit.form.CheckBox' type='checkbox'>
			</td>
			<td>
				<?= htmlspecialchars($pass["title"]) ?>
			</td>
			<td class='text-muted'>
				<?= make_local_datetime($pass['created']) ?>
			</td>
			<td class='text-muted'>
				<?= make_local_datetime($pass['last_used']) ?>
			</td>
		</tr>
		<?php } ?>
	</table>
</div>
