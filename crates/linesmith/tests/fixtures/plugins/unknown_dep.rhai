// @data_deps = ["credentials"]
//
// `credentials` is reserved per plugin-api.md §@data_deps header
// syntax — exposing OAuth token storage to scripts is a leak path.
// The header parser rejects this with HeaderError::UnknownDep at
// load time, surfacing as PluginError::UnknownDataDep.

const ID = "unknown_dep";

fn render(ctx) {
    ()
}
