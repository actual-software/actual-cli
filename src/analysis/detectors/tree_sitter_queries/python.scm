; Python tree-sitter queries for architecture facet extraction

; =============================================================================
; PUBLIC API - Functions and Classes (leaf_id: 25)
; =============================================================================

; @rule_id: ts.py.public_function
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(function_definition
  name: (identifier) @fn_name
  (#not-match? @fn_name "^_"))

; @rule_id: ts.py.public_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(class_definition
  name: (identifier) @class_name
  (#not-match? @class_name "^_"))

; @rule_id: ts.py.async_function
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(function_definition
  "async"
  name: (identifier) @fn_name
  (#not-match? @fn_name "^_"))

; =============================================================================
; IMPORTS - Dependencies (leaf_id: 20)
; =============================================================================

; @rule_id: ts.py.import_statement
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(import_statement
  name: (dotted_name) @import_name)

; @rule_id: ts.py.import_from
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(import_from_statement
  module_name: (dotted_name) @module_name)

; =============================================================================
; FRAMEWORK PATTERNS - FastAPI/Flask/Django (leaf_id: 19)
; =============================================================================

; @rule_id: ts.py.fastapi_route
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.95
(decorated_definition
  (decorator
    (call
      function: (attribute
        object: (identifier) @obj
        attribute: (identifier) @method)
      (#match? @method "^(get|post|put|delete|patch|options|head)$"))))

; @rule_id: ts.py.flask_route
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.9
(decorated_definition
  (decorator
    (call
      function: (attribute
        object: (identifier) @obj
        attribute: (identifier) @method (#eq? @method "route")))))

; @rule_id: ts.py.django_view
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.85
(class_definition
  name: (identifier) @class_name
  superclasses: (argument_list
    (identifier) @base (#match? @base "View$")))

; =============================================================================
; DEPENDENCY INJECTION (leaf_id: 19)
; =============================================================================

; @rule_id: ts.py.fastapi_depends
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.9
(call
  function: (identifier) @fn (#eq? @fn "Depends"))

; =============================================================================
; DATA MODELS - Pydantic/SQLAlchemy (leaf_id: 29, 30)
; =============================================================================

; @rule_id: ts.py.pydantic_model
; @facet_slot: domain.validation
; @leaf_id: 56
; @confidence: 0.9
(class_definition
  name: (identifier) @class_name
  superclasses: (argument_list
    (identifier) @base (#eq? @base "BaseModel")))

; @rule_id: ts.py.sqlalchemy_model
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.9
(class_definition
  name: (identifier) @class_name
  superclasses: (argument_list
    (identifier) @base (#eq? @base "Base")))

; @rule_id: ts.py.dataclass
; @facet_slot: domain.modeling_style
; @leaf_id: 55
; @confidence: 0.85
(decorated_definition
  (decorator
    (identifier) @dec (#eq? @dec "dataclass")))

; =============================================================================
; ASYNC/CONCURRENCY (leaf_id: 17)
; =============================================================================

; @rule_id: ts.py.async_def
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(function_definition
  "async"
  name: (identifier) @fn_name)

; @rule_id: ts.py.await_expression
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.85
(await
  (call) @awaited_call)

; =============================================================================
; CONFIGURATION (leaf_id: 37)
; =============================================================================

; @rule_id: ts.py.os_environ
; @facet_slot: runtime.config.sources
; @leaf_id: 37
; @confidence: 0.9
(subscript
  value: (attribute
    object: (identifier) @obj (#eq? @obj "os")
    attribute: (identifier) @attr (#eq? @attr "environ"))
  subscript: (string) @env_var)

; @rule_id: ts.py.os_getenv
; @facet_slot: runtime.config.sources
; @leaf_id: 37
; @confidence: 0.9
(call
  function: (attribute
    object: (identifier) @obj (#eq? @obj "os")
    attribute: (identifier) @method (#eq? @method "getenv"))
  arguments: (argument_list
    (string) @env_var))

; =============================================================================
; OBSERVABILITY - Logging (leaf_id: 46)
; =============================================================================

; @rule_id: ts.py.logging_getlogger
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.9
(call
  function: (attribute
    object: (identifier) @obj (#eq? @obj "logging")
    attribute: (identifier) @method (#eq? @method "getLogger")))

; @rule_id: ts.py.structlog
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.9
(call
  function: (attribute
    object: (identifier) @obj (#eq? @obj "structlog")
    attribute: (identifier) @method (#eq? @method "get_logger")))

; =============================================================================
; TESTING (leaf_id: 39)
; =============================================================================

; @rule_id: ts.py.pytest_function
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.9
(function_definition
  name: (identifier) @fn_name
  (#match? @fn_name "^test_"))

; @rule_id: ts.py.pytest_fixture
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.9
(decorated_definition
  (decorator
    (attribute
      object: (identifier) @obj (#eq? @obj "pytest")
      attribute: (identifier) @attr (#eq? @attr "fixture"))))

; =============================================================================
; DATABASE CLIENTS (leaf_id: 28)
; =============================================================================

; @rule_id: ts.py.asyncpg
; @facet_slot: data.primary_datastores
; @leaf_id: 28
; @confidence: 0.9
(call
  function: (attribute
    object: (identifier) @obj (#eq? @obj "asyncpg")
    attribute: (identifier) @method (#eq? @method "connect")))

; @rule_id: ts.py.psycopg
; @facet_slot: data.primary_datastores
; @leaf_id: 28
; @confidence: 0.9
(call
  function: (attribute
    object: (identifier) @obj (#match? @obj "^psycopg")
    attribute: (identifier) @method (#eq? @method "connect")))
