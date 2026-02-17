; PHP tree-sitter queries for architecture facet extraction
; Supports Laravel, Eloquent, and general PHP patterns

; =============================================================================
; PUBLIC API - Classes and Functions (leaf_id: 25)
; =============================================================================

; @rule_id: ts.php.public_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(class_declaration
  name: (name) @class_name)

; @rule_id: ts.php.public_function
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(function_definition
  name: (name) @fn_name)

; @rule_id: ts.php.interface_declaration
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(interface_declaration
  name: (name) @interface_name)

; @rule_id: ts.php.trait_declaration
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(trait_declaration
  name: (name) @trait_name)

; =============================================================================
; IMPORTS - use statements (leaf_id: 20)
; =============================================================================

; @rule_id: ts.php.use_statement
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(namespace_use_clause
  (qualified_name) @import_name)

; @rule_id: ts.php.namespace
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.85
(namespace_definition
  name: (namespace_name) @namespace_name)

; =============================================================================
; LARAVEL - Route definitions (leaf_id: 23)
; =============================================================================

; @rule_id: ts.php.laravel_route
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.9
(member_call_expression
  object: (name) @obj (#eq? @obj "Route")
  name: (name) @method (#match? @method "^(get|post|put|patch|delete|options|any|match|resource|apiResource)$"))

; @rule_id: ts.php.laravel_route_scoped
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.85
(scoped_call_expression
  scope: (name) @scope (#eq? @scope "Route")
  name: (name) @method (#match? @method "^(get|post|put|patch|delete|options|any|match|resource|apiResource)$"))

; =============================================================================
; ELOQUENT - Model definitions (leaf_id: 30)
; =============================================================================

; @rule_id: ts.php.eloquent_model
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.9
(class_declaration
  name: (name) @class_name
  (base_clause
    (name) @base (#eq? @base "Model")))

; @rule_id: ts.php.eloquent_query_builder
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.85
(member_call_expression
  name: (name) @method (#match? @method "^(where|find|first|get|all|create|update|delete|save|orderBy|groupBy|join|with|load)$"))

; =============================================================================
; DATABASE - PDO and Query Builder (leaf_id: 28)
; =============================================================================

; @rule_id: ts.php.pdo_new
; @facet_slot: data.primary_datastores
; @leaf_id: 28
; @confidence: 0.95
(object_creation_expression
  (name) @class (#eq? @class "PDO"))

; @rule_id: ts.php.db_facade
; @facet_slot: data.primary_datastores
; @leaf_id: 28
; @confidence: 0.9
(scoped_call_expression
  scope: (name) @scope (#eq? @scope "DB")
  name: (name) @method (#match? @method "^(table|select|insert|update|delete|statement|connection)$"))

; =============================================================================
; AUTHENTICATION (leaf_id: 35)
; =============================================================================

; @rule_id: ts.php.laravel_auth
; @facet_slot: authn.methods
; @leaf_id: 35
; @confidence: 0.9
(scoped_call_expression
  scope: (name) @scope (#eq? @scope "Auth")
  name: (name) @method (#match? @method "^(attempt|login|logout|user|check|guard|id)$"))

; @rule_id: ts.php.password_hash
; @facet_slot: authn.credential_storage
; @leaf_id: 35
; @confidence: 0.95
(function_call_expression
  function: (name) @fn (#match? @fn "^(password_hash|password_verify)$"))

; @rule_id: ts.php.laravel_hash
; @facet_slot: authn.credential_storage
; @leaf_id: 35
; @confidence: 0.9
(scoped_call_expression
  scope: (name) @scope (#eq? @scope "Hash")
  name: (name) @method (#match? @method "^(make|check|needsRehash)$"))

; =============================================================================
; LOGGING (leaf_id: 46)
; =============================================================================

; @rule_id: ts.php.laravel_log
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.9
(scoped_call_expression
  scope: (name) @scope (#eq? @scope "Log")
  name: (name) @method (#match? @method "^(info|error|warning|debug|critical|alert|notice|emergency)$"))

; @rule_id: ts.php.monolog_logger
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.85
(object_creation_expression
  (name) @class (#eq? @class "Logger"))

; =============================================================================
; VALIDATION (leaf_id: 37)
; =============================================================================

; @rule_id: ts.php.laravel_validator
; @facet_slot: security.input_validation
; @leaf_id: 37
; @confidence: 0.9
(scoped_call_expression
  scope: (name) @scope (#eq? @scope "Validator")
  name: (name) @method (#eq? @method "make"))

; @rule_id: ts.php.filter_var
; @facet_slot: security.input_validation
; @leaf_id: 37
; @confidence: 0.85
(function_call_expression
  function: (name) @fn (#eq? @fn "filter_var"))

; =============================================================================
; ENCRYPTION (leaf_id: 39)
; =============================================================================

; @rule_id: ts.php.laravel_crypt
; @facet_slot: security.encryption
; @leaf_id: 39
; @confidence: 0.9
(scoped_call_expression
  scope: (name) @scope (#eq? @scope "Crypt")
  name: (name) @method (#match? @method "^(encrypt|decrypt|encryptString|decryptString)$"))

; @rule_id: ts.php.openssl
; @facet_slot: security.encryption
; @leaf_id: 39
; @confidence: 0.9
(function_call_expression
  function: (name) @fn (#match? @fn "^(openssl_encrypt|openssl_decrypt|openssl_cipher_iv_length)$"))

; =============================================================================
; TESTING (leaf_id: 50)
; =============================================================================

; @rule_id: ts.php.phpunit_testcase
; @facet_slot: testing.unit
; @leaf_id: 50
; @confidence: 0.95
(class_declaration
  name: (name) @class_name
  (base_clause
    (name) @base (#match? @base "TestCase$")))

; @rule_id: ts.php.phpunit_assertion
; @facet_slot: testing.unit
; @leaf_id: 50
; @confidence: 0.9
(member_call_expression
  object: (variable_name) @obj (#eq? @obj "$this")
  name: (name) @method (#match? @method "^(assert|expect)"))

; =============================================================================
; MIDDLEWARE (leaf_id: 24)
; =============================================================================

; @rule_id: ts.php.laravel_middleware
; @facet_slot: boundaries.middleware
; @leaf_id: 24
; @confidence: 0.9
(method_declaration
  name: (name) @method (#eq? @method "handle")
  parameters: (formal_parameters
    (simple_parameter
      type: (named_type
        (name) @type (#eq? @type "Request")))))

; =============================================================================
; SESSION (leaf_id: 35)
; =============================================================================

; @rule_id: ts.php.session_start
; @facet_slot: authn.session_management
; @leaf_id: 35
; @confidence: 0.85
(function_call_expression
  function: (name) @fn (#eq? @fn "session_start"))

; @rule_id: ts.php.session_superglobal
; @facet_slot: authn.session_management
; @leaf_id: 35
; @confidence: 0.8
(subscript_expression
  (variable_name) @var (#eq? @var "$_SESSION"))

; =============================================================================
; ASYNC/QUEUE (leaf_id: 17)
; =============================================================================

; @rule_id: ts.php.laravel_dispatch
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(function_call_expression
  function: (name) @fn (#eq? @fn "dispatch"))

; @rule_id: ts.php.laravel_queue
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.85
(scoped_call_expression
  scope: (name) @scope (#eq? @scope "Queue")
  name: (name) @method (#match? @method "^(push|later|bulk)$"))
