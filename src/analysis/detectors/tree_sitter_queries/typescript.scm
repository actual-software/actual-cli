; TypeScript tree-sitter queries for architecture facet extraction
; Extends JavaScript patterns with TypeScript-specific constructs

; =============================================================================
; PUBLIC API - Type Exports (leaf_id: 25)
; =============================================================================

; @rule_id: ts.ts.export_interface
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(export_statement
  (interface_declaration
    name: (type_identifier) @export_name))

; @rule_id: ts.ts.export_type_alias
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(export_statement
  (type_alias_declaration
    name: (type_identifier) @export_name))

; @rule_id: ts.ts.export_enum
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(export_statement
  (enum_declaration
    name: (identifier) @export_name))

; @rule_id: ts.ts.export_function
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(export_statement
  (function_declaration
    name: (identifier) @export_name))

; @rule_id: ts.ts.export_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(export_statement
  (class_declaration
    name: (type_identifier) @export_name))

; @rule_id: ts.ts.export_const
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(export_statement
  (lexical_declaration
    (variable_declarator
      name: (identifier) @export_name)))

; @rule_id: ts.ts.export_default
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(export_statement
  "default"
  (_) @export_value)

; =============================================================================
; IMPORTS - Dependencies (leaf_id: 20)
; =============================================================================

; @rule_id: ts.ts.import_declaration
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(import_statement
  source: (string) @import_source)

; @rule_id: ts.ts.type_import
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.85
(import_statement
  "type"
  source: (string) @import_source)

; =============================================================================
; FRAMEWORK PATTERNS - NestJS decorators (leaf_id: 19)
; =============================================================================

; @rule_id: ts.ts.nestjs_controller
; @facet_slot: framework.app.detected
; @leaf_id: 19
; @confidence: 0.95
(decorator
  (call_expression
    function: (identifier) @dec (#eq? @dec "Controller")))

; @rule_id: ts.ts.nestjs_injectable
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.9
(decorator
  (call_expression
    function: (identifier) @dec (#eq? @dec "Injectable")))

; @rule_id: ts.ts.nestjs_module
; @facet_slot: framework.app.detected
; @leaf_id: 19
; @confidence: 0.95
(decorator
  (call_expression
    function: (identifier) @dec (#eq? @dec "Module")))

; @rule_id: ts.ts.nestjs_get
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.9
(decorator
  (call_expression
    function: (identifier) @dec (#match? @dec "^(Get|Post|Put|Delete|Patch|Options|Head|All)$")))

; =============================================================================
; SERVICE BOUNDARIES - Express/Fastify routes (leaf_id: 23)
; =============================================================================

; @rule_id: ts.ts.express_route
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.9
(call_expression
  function: (member_expression
    object: (identifier) @obj
    property: (property_identifier) @method)
  (#match? @method "^(get|post|put|delete|patch|options|head|all|use)$")
  arguments: (arguments
    (string) @route_path))

; =============================================================================
; ASYNC/CONCURRENCY PATTERNS (leaf_id: 17)
; =============================================================================

; @rule_id: ts.ts.async_function
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(function_declaration
  "async"
  name: (identifier) @fn_name)

; @rule_id: ts.ts.async_method
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(method_definition
  "async"
  name: (property_identifier) @method_name)

; =============================================================================
; OBSERVABILITY - Logging (leaf_id: 46)
; =============================================================================

; @rule_id: ts.ts.pino_logger
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.9
(call_expression
  function: (identifier) @fn (#eq? @fn "pino"))

; @rule_id: ts.ts.winston_logger
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.9
(call_expression
  function: (member_expression
    object: (identifier) @obj (#eq? @obj "winston")
    property: (property_identifier) @method (#eq? @method "createLogger")))

; =============================================================================
; DATABASE CLIENTS (leaf_id: 28)
; =============================================================================

; @rule_id: ts.ts.prisma_client
; @facet_slot: data.primary_datastores
; @leaf_id: 28
; @confidence: 0.95
(new_expression
  constructor: (identifier) @cls (#eq? @cls "PrismaClient"))

; @rule_id: ts.ts.typeorm_entity
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.9
(decorator
  (call_expression
    function: (identifier) @dec (#eq? @dec "Entity")))

; @rule_id: ts.ts.typeorm_repository
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.9
(call_expression
  function: (identifier) @fn (#eq? @fn "getRepository"))

; =============================================================================
; CONFIGURATION (leaf_id: 37)
; =============================================================================

; @rule_id: ts.ts.process_env
; @facet_slot: runtime.config.sources
; @leaf_id: 37
; @confidence: 0.9
(member_expression
  object: (member_expression
    object: (identifier) @obj (#eq? @obj "process")
    property: (property_identifier) @prop (#eq? @prop "env"))
  property: (property_identifier) @env_var)

; =============================================================================
; TESTING (leaf_id: 39)
; =============================================================================

; @rule_id: ts.ts.jest_describe
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.9
(call_expression
  function: (identifier) @fn (#eq? @fn "describe")
  arguments: (arguments
    (string) @test_name))

; @rule_id: ts.ts.jest_it
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.9
(call_expression
  function: (identifier) @fn (#match? @fn "^(it|test)$")
  arguments: (arguments
    (string) @test_name))
