use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
/// Template engine for rendering skill content with arguments.
pub struct TemplateEngine;

impl TemplateEngine {
    /// Create a new template engine.
    pub fn new() -> Self {
        Self
    }
    
    /// Render a template string with the given context.
    pub fn render(&self, template: &str, context: &RenderContext) -> Result<String> {
        let mut result = template.to_string();
        
        // Replace {{arg.name}} with argument values
        for (name, value) in &context.arguments {
            let placeholder = format!("{{{{{}}}}}", name);
            let replacement = self.value_to_string(value)?;
            result = result.replace(&placeholder, &replacement);
        }
        
        // Replace {{arg.name:default}} with default if not provided
        // This is handled by the context providing defaults
        
        // Replace {{#if arg.name}}...{{/if}} conditional blocks
        result = self.render_conditionals(&result, context)?;
        
        // Replace {{#each arg.name}}...{{/each}} loop blocks
        result = self.render_loops(&result, context)?;
        
        Ok(result)
    }
    
    /// Render conditional blocks {{#if name}}...{{/if}}
    fn render_conditionals(&self, template: &str, context: &RenderContext) -> Result<String> {
        let mut result = template.to_string();
        
        // Pattern: {{#if name}}content{{/if}}
        let re = regex::Regex::new(r"\{\{#if\s+(\w+)\}\}(.*?)\{\{/if\}\}")?;
        
        loop {
            let mut changed = false;
            result = re.replace_all(&result, |caps: &regex::Captures| {
                let name = &caps[1];
                let content = &caps[2];
                
                if let Some(value) = context.arguments.get(name) {
                    if self.is_truthy(value) {
                        changed = true;
                        content.to_string()
                    } else {
                        changed = true;
                        String::new()
                    }
                } else {
                    changed = true;
                    String::new()
                }
            }).to_string();
            
            if !changed {
                break;
            }
        }
        
        Ok(result)
    }
    
    /// Render loop blocks {{#each name}}...{{/each}}
    fn render_loops(&self, template: &str, context: &RenderContext) -> Result<String> {
        let mut result = template.to_string();
        
        // Pattern: {{#each name}}content{{/each}}
        let re = regex::Regex::new(r"\{\{#each\s+(\w+)\}\}(.*?)\{\{/each\}\}")?;
        
        loop {
            let mut changed = false;
            result = re.replace_all(&result, |caps: &regex::Captures| {
                let name = &caps[1];
                let content = &caps[2];
                
                if let Some(Value::Array(items)) = context.arguments.get(name) {
                    changed = true;
                    items.iter().map(|item| {
                        let mut item_result = content.to_string();
                        // Replace {{this}} with the item value
                        item_result = item_result.replace("{{this}}", &self.value_to_string(item).unwrap_or_default());
                        // Replace {{@index}} with index
                        // This would need index tracking - simplified for now
                        item_result
                    }).collect::<Vec<_>>().join("")
                } else {
                    changed = true;
                    String::new()
                }
            }).to_string();
            
            if !changed {
                break;
            }
        }
        
        Ok(result)
    }
    
    /// Convert a JSON value to a string for template substitution.
    fn value_to_string(&self, value: &Value) -> Result<String> {
        match value {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            Value::Null => Ok(String::new()),
            Value::Array(arr) => Ok(serde_json::to_string(arr)?),
            Value::Object(obj) => Ok(serde_json::to_string(obj)?),
        }
    }
    
    /// Check if a value is truthy for conditionals.
    fn is_truthy(&self, value: &Value) -> bool {
        match value {
            Value::Bool(b) => *b,
            Value::String(s) => !s.is_empty(),
            Value::Number(n) => n.as_f64().map_or(false, |f| f != 0.0),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            Value::Null => false,
        }
    }
}

impl Default for TemplateEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Rendering context with arguments and metadata.
#[derive(Debug, Clone, Default)]
pub struct RenderContext {
    /// Arguments provided to the skill
    pub arguments: HashMap<String, Value>,
    
    /// Skill metadata
    pub metadata: HashMap<String, Value>,
    
    /// Current working directory
    pub cwd: Option<String>,
    
    /// Environment variables
    pub env: HashMap<String, String>,
}

impl RenderContext {
    /// Create a new render context.
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Add an argument.
    pub fn with_arg(mut self, name: impl Into<String>, value: Value) -> Self {
        self.arguments.insert(name.into(), value);
        self
    }
    
    /// Add multiple arguments.
    pub fn with_args(mut self, args: HashMap<String, Value>) -> Self {
        self.arguments.extend(args);
        self
    }
    
    /// Add metadata.
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
    
    /// Set working directory.
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
    
    /// Add environment variable.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    
    #[test]
    fn test_simple_render() {
        let engine = TemplateEngine::new();
        let context = RenderContext::new()
            .with_arg("name", json!("World"))
            .with_arg("count", json!(42));
        
        let result = engine.render("Hello {{name}}! Count: {{count}}", &context).unwrap();
        assert_eq!(result, "Hello World! Count: 42");
    }
    
    #[test]
    fn test_conditional_render() {
        let engine = TemplateEngine::new();
        
        let context_true = RenderContext::new().with_arg("show", json!(true));
        let result = engine.render("{{#if show}}Visible{{/if}}", &context_true).unwrap();
        assert_eq!(result, "Visible");
        
        let context_false = RenderContext::new().with_arg("show", json!(false));
        let result = engine.render("{{#if show}}Visible{{/if}}", &context_false).unwrap();
        assert_eq!(result, "");
    }
    
    #[test]
    fn test_loop_render() {
        let engine = TemplateEngine::new();
        let context = RenderContext::new().with_arg("items", json!(["a", "b", "c"]));
        
        let result = engine.render("{{#each items}}- {{this}}\n{{/each}}", &context).unwrap();
        assert_eq!(result, "- a\n- b\n- c\n");
    }
    
    #[test]
    fn test_empty_string_for_missing() {
        let engine = TemplateEngine::new();
        let context = RenderContext::new();
        
        let result = engine.render("Hello {{name}}!", &context).unwrap();
        assert_eq!(result, "Hello !");
    }
}