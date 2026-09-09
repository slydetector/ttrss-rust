<?php
// Standalone ground-truth generator for Pref_Filters::index (the filters tab
// page). Replicates the method's HTML verbatim (clean/__ = app behavior; the
// PluginHost hook is a no-op with no plugins). Reads the search from argv so
// we can capture several variants.

function clean(mixed $param): mixed {
    $filter = static fn($v) => is_string($v) ? trim(strip_tags($v)) : $v;
    return is_array($param) ? array_map($filter, $param) : $filter($param);
}
function __($s) { return $s; }

$search = $argv[1] ?? "";
$_REQUEST = $search === "" ? [] : ["search" => $search];
$_SESSION = [];

if (array_key_exists("search", $_REQUEST)) {
    $filter_search = clean($_REQUEST["search"]);
    $_SESSION["prefs_filter_search"] = $filter_search;
} else {
    $filter_search = ($_SESSION["prefs_filter_search"] ?? "");
}

?>
		<div dojoType='dijit.layout.BorderContainer' gutters='false'>
			<div style='padding : 0px' dojoType='dijit.layout.ContentPane' region='top'>
				<div dojoType='fox.Toolbar'>

					<div style='float : right; padding-right : 4px;'>
						<form dojoType="dijit.form.Form" onsubmit="dijit.byId('filterTree').reload(); return false;">
							<input dojoType="dijit.form.TextBox" id="filter_search" size="20" type="search"
								value="<?= htmlspecialchars($filter_search) ?>">
							<button dojoType="dijit.form.Button" type="submit">
								<?= __('Search') ?></button>
						</form>
					</div>

					<div dojoType="fox.form.DropDownButton">
						<span><?= __('Select') ?></span>
						<div dojoType="dijit.Menu" style="display: none;">
							<div onclick="dijit.byId('filterTree').model.setAllChecked(true)"
								dojoType="dijit.MenuItem"><?= __('All') ?></div>
							<div onclick="dijit.byId('filterTree').model.setAllChecked(false)"
								dojoType="dijit.MenuItem"><?= __('None') ?></div>
						</div>
					</div>

					<button dojoType="dijit.form.Button" onclick="return Filters.edit()">
						<?= __('Create filter') ?></button>
					<button dojoType="dijit.form.Button" onclick="return dijit.byId('filterTree').cloneSelectedFilters()">
						<?= __('Clone') ?></button>
					<button dojoType="dijit.form.Button" onclick="return dijit.byId('filterTree').joinSelectedFilters()">
						<?= __('Combine') ?></button>
					<button dojoType="dijit.form.Button" onclick="return dijit.byId('filterTree').removeSelectedFilters()">
						<?= __('Remove') ?></button>
					<button dojoType="dijit.form.Button" onclick="return dijit.byId('filterTree').resetFilterOrder()">
						<?= __('Reset sort order') ?></button>
					<button dojoType="dijit.form.Button" onclick="return dijit.byId('filterTree').toggleRules()">
						<?= __('Toggle rule display') ?></button>

				</div>
			</div>
			<div style='padding : 0px' dojoType='dijit.layout.ContentPane' region='center'>
				<div dojoType="fox.PrefFilterStore" jsId="filterStore"
					url="backend.php?op=Pref_Filters&method=getfiltertree">
				</div>
				<div dojoType="lib.CheckBoxStoreModel" jsId="filterModel" store="filterStore"
					query="{id:'root'}" rootId="root" rootLabel="Filters"
					childrenAttrs="items" checkboxStrict="false" checkboxAll="false">
				</div>
				<div dojoType="fox.PrefFilterTree" id="filterTree" dndController="dijit.tree.dndSource"
					betweenThreshold="5" model="filterModel" openOnClick="true">
				</div>
			</div>
			<?php /* PluginHost hook: no-op */ ?>
		</div>
