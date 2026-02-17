; C# tree-sitter queries for architecture facet extraction

; =============================================================================
; PUBLIC API - Classes, Interfaces, Methods (leaf_id: 25)
; =============================================================================

; @rule_id: ts.csharp.public_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(class_declaration
  (modifier) @mod (#eq? @mod "public")
  name: (identifier) @class_name)

; @rule_id: ts.csharp.public_interface
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(interface_declaration
  (modifier) @mod (#eq? @mod "public")
  name: (identifier) @interface_name)

; @rule_id: ts.csharp.public_struct
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(struct_declaration
  (modifier) @mod (#eq? @mod "public")
  name: (identifier) @struct_name)

; @rule_id: ts.csharp.public_method
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(method_declaration
  (modifier) @mod (#eq? @mod "public")
  name: (identifier) @method_name)

; =============================================================================
; IMPORTS - Dependencies (leaf_id: 20)
; =============================================================================

; @rule_id: ts.csharp.using_directive
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.85
(using_directive
  (qualified_name) @import_path)

; @rule_id: ts.csharp.using_directive_simple
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.8
(using_directive
  (identifier) @import_path)

; =============================================================================
; SERVICE BOUNDARIES - ASP.NET attributes (leaf_id: 23)
; =============================================================================

; @rule_id: ts.csharp.aspnet_api_controller
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.9
(attribute
  name: (identifier) @ann (#eq? @ann "ApiController"))

; @rule_id: ts.csharp.aspnet_route
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.85
(attribute
  name: (identifier) @ann (#match? @ann "^(HttpGet|HttpPost|HttpPut|HttpDelete|HttpPatch|Route)$"))

; =============================================================================
; AUTHORIZATION - ASP.NET attributes (leaf_id: 36)
; =============================================================================

; @rule_id: ts.csharp.aspnet_authorize
; @facet_slot: authz.model
; @leaf_id: 36
; @confidence: 0.9
(attribute
  name: (identifier) @ann (#eq? @ann "Authorize"))

; =============================================================================
; DATA MODELING - EF Core attributes (leaf_id: 29)
; =============================================================================

; @rule_id: ts.csharp.data_table_attribute
; @facet_slot: data.modeling.style
; @leaf_id: 29
; @confidence: 0.85
(attribute
  name: (identifier) @ann (#eq? @ann "Table"))

; @rule_id: ts.csharp.data_key_attribute
; @facet_slot: data.modeling.style
; @leaf_id: 29
; @confidence: 0.8
(attribute
  name: (identifier) @ann (#eq? @ann "Key"))
