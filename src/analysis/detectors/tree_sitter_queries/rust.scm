; Rust tree-sitter queries for architecture facet extraction

; =============================================================================
; PUBLIC API - Exported items (leaf_id: 25)
; =============================================================================

; @rule_id: ts.rust.pub_function
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(function_item
  (visibility_modifier) @vis (#eq? @vis "pub")
  name: (identifier) @fn_name)

; @rule_id: ts.rust.pub_struct
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(struct_item
  (visibility_modifier) @vis (#eq? @vis "pub")
  name: (type_identifier) @struct_name)

; @rule_id: ts.rust.pub_enum
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(enum_item
  (visibility_modifier) @vis (#eq? @vis "pub")
  name: (type_identifier) @enum_name)

; @rule_id: ts.rust.pub_trait
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.95
(trait_item
  (visibility_modifier) @vis (#eq? @vis "pub")
  name: (type_identifier) @trait_name)

; @rule_id: ts.rust.pub_mod
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(mod_item
  (visibility_modifier) @vis (#eq? @vis "pub")
  name: (identifier) @mod_name)

; =============================================================================
; IMPORTS - Dependencies (leaf_id: 20)
; =============================================================================

; @rule_id: ts.rust.use_declaration
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(use_declaration
  argument: (scoped_identifier) @import_path)

; @rule_id: ts.rust.extern_crate
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(extern_crate_declaration
  name: (identifier) @crate_name)

; =============================================================================
; FRAMEWORK PATTERNS - Actix/Axum (leaf_id: 19, 23)
; =============================================================================

; @rule_id: ts.rust.actix_route
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.95
(attribute_item
  (attribute
    (identifier) @attr (#match? @attr "^(get|post|put|delete|patch|head|options)$")))

; @rule_id: ts.rust.derive_attribute
; @facet_slot: domain.modeling_style
; @leaf_id: 55
; @confidence: 0.85
(attribute_item
  (attribute
    (identifier) @attr (#eq? @attr "derive")))

; =============================================================================
; ASYNC/CONCURRENCY (leaf_id: 17)
; =============================================================================

; @rule_id: ts.rust.async_function
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(function_item
  (function_modifiers "async")
  name: (identifier) @fn_name)

; @rule_id: ts.rust.await_expression
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.85
(await_expression) @await_expr

; =============================================================================
; DATA - Serde serialization (leaf_id: 29)
; =============================================================================

; @rule_id: ts.rust.serde_derive
; @facet_slot: data.modeling.style
; @leaf_id: 29
; @confidence: 0.9
(attribute_item
  (attribute
    (identifier) @attr (#eq? @attr "derive")
    arguments: (token_tree) @args)
  (#match? @args "Serialize|Deserialize"))

; =============================================================================
; TESTING (leaf_id: 39)
; =============================================================================

; @rule_id: ts.rust.test_function
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.95
(attribute_item
  (attribute
    (identifier) @attr (#eq? @attr "test")))

; @rule_id: ts.rust.test_module
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.9
((attribute_item
  (attribute
    (identifier) @attr (#eq? @attr "cfg")
    arguments: (token_tree) @args))
 (#match? @args "test"))

; =============================================================================
; OBSERVABILITY - Tracing (leaf_id: 46, 48)
; =============================================================================

; @rule_id: ts.rust.tracing_instrument
; @facet_slot: obs.tracing
; @leaf_id: 48
; @confidence: 0.9
(attribute_item
  (attribute
    (identifier) @attr (#eq? @attr "instrument")))

; @rule_id: ts.rust.tracing_macro
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.85
(macro_invocation
  macro: (identifier) @macro (#match? @macro "^(info|debug|warn|error|trace)!?$"))
