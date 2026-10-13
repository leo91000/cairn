-- Authoring reference for the v1 host call bridge. Runtime globals are host-owned.
export type Json = boolean | number | string | {Json} | {[string]: Json}
export type Cairn = {
    call: (method: string, args: Json) -> Json,
}
return {}
