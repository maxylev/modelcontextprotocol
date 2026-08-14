use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// An entity (node) in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[schemars(description = "An entity in the knowledge graph")]
pub struct Entity {
    /// The name of the entity
    #[schemars(description = "The name of the entity")]
    pub name: String,
    /// The type of the entity
    #[schemars(description = "The type of the entity")]
    pub entity_type: String,
    /// An array of observation contents associated with the entity
    #[schemars(description = "An array of observation contents associated with the entity")]
    pub observations: Vec<String>,
}

/// A directed relation between two entities.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[schemars(description = "A directed relation between two entities")]
pub struct Relation {
    /// The name of the entity where the relation starts
    #[schemars(description = "The name of the entity where the relation starts")]
    pub from: String,
    /// The name of the entity where the relation ends
    #[schemars(description = "The name of the entity where the relation ends")]
    pub to: String,
    /// The type of the relation
    #[schemars(description = "The type of the relation")]
    pub relation_type: String,
}

/// The full knowledge graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[schemars(description = "The knowledge graph")]
pub struct KnowledgeGraph {
    pub entities: Vec<Entity>,
    pub relations: Vec<Relation>,
}

/// Input for `add_observations`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(description = "Observations to add to an entity")]
pub struct ObservationInput {
    /// The name of the entity to add the observations to
    #[schemars(description = "The name of the entity to add the observations to")]
    pub entity_name: String,
    /// An array of observation contents to add
    #[schemars(description = "An array of observation contents to add")]
    pub contents: Vec<String>,
}

/// Result of `add_observations`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(description = "The result of adding observations to an entity")]
pub struct AddedObservation {
    pub entity_name: String,
    pub added_observations: Vec<String>,
}

/// One line in the JSONL storage file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum GraphItem {
    Entity(Entity),
    Relation(Relation),
}

/// Persists the knowledge graph as a JSONL file, mirroring the reference
/// TypeScript server's `KnowledgeGraphManager`: every mutation loads the
/// current graph, applies the change, and rewrites the whole file.
pub struct KnowledgeGraphManager {
    path: PathBuf,
    gate: Mutex<()>,
}

impl KnowledgeGraphManager {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            gate: Mutex::new(()),
        }
    }

    async fn load(&self) -> Result<KnowledgeGraph, String> {
        let data = match tokio::fs::read_to_string(&self.path).await {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(KnowledgeGraph::default());
            }
            Err(e) => return Err(format!("Failed to read {}: {e}", self.path.display())),
        };

        let mut graph = KnowledgeGraph::default();
        for line in data.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let item: GraphItem = serde_json::from_str(line)
                .map_err(|e| format!("Corrupt memory file {}: {e}", self.path.display()))?;
            match item {
                GraphItem::Entity(entity) => graph.entities.push(entity),
                GraphItem::Relation(relation) => graph.relations.push(relation),
            }
        }
        Ok(graph)
    }

    async fn save(&self, graph: &KnowledgeGraph) -> Result<(), String> {
        let mut lines = Vec::with_capacity(graph.entities.len() + graph.relations.len());
        for entity in &graph.entities {
            lines.push(
                serde_json::to_string(&GraphItem::Entity(entity.clone()))
                    .map_err(|e| format!("Failed to serialize entity: {e}"))?,
            );
        }
        for relation in &graph.relations {
            lines.push(
                serde_json::to_string(&GraphItem::Relation(relation.clone()))
                    .map_err(|e| format!("Failed to serialize relation: {e}"))?,
            );
        }
        tokio::fs::write(&self.path, lines.join("\n"))
            .await
            .map_err(|e| format!("Failed to write {}: {e}", self.path.display()))
    }

    /// Create entities, ignoring names that already exist. Returns the
    /// entities that were actually added.
    pub async fn create_entities(&self, entities: Vec<Entity>) -> Result<Vec<Entity>, String> {
        let _guard = self.gate.lock().await;
        let mut graph = self.load().await?;
        let new_entities: Vec<Entity> = entities
            .into_iter()
            .filter(|e| {
                !graph
                    .entities
                    .iter()
                    .any(|existing| existing.name == e.name)
            })
            .collect();
        graph.entities.extend(new_entities.clone());
        self.save(&graph).await?;
        Ok(new_entities)
    }

    /// Create relations, skipping exact duplicates. Returns the relations
    /// that were actually added.
    pub async fn create_relations(
        &self,
        relations: Vec<Relation>,
    ) -> Result<Vec<Relation>, String> {
        let _guard = self.gate.lock().await;
        let mut graph = self.load().await?;
        let new_relations: Vec<Relation> = relations
            .into_iter()
            .filter(|r| {
                !graph.relations.iter().any(|existing| {
                    existing.from == r.from
                        && existing.to == r.to
                        && existing.relation_type == r.relation_type
                })
            })
            .collect();
        graph.relations.extend(new_relations.clone());
        self.save(&graph).await?;
        Ok(new_relations)
    }

    /// Add observations to entities. Fails if any entity does not exist.
    pub async fn add_observations(
        &self,
        observations: Vec<ObservationInput>,
    ) -> Result<Vec<AddedObservation>, String> {
        let _guard = self.gate.lock().await;
        let mut graph = self.load().await?;
        let mut results = Vec::with_capacity(observations.len());
        for input in observations {
            let entity = graph
                .entities
                .iter_mut()
                .find(|e| e.name == input.entity_name)
                .ok_or_else(|| format!("Entity with name {} not found", input.entity_name))?;
            let new_observations: Vec<String> = input
                .contents
                .into_iter()
                .filter(|content| !entity.observations.contains(content))
                .collect();
            entity.observations.extend(new_observations.clone());
            results.push(AddedObservation {
                entity_name: entity.name.clone(),
                added_observations: new_observations,
            });
        }
        self.save(&graph).await?;
        Ok(results)
    }

    /// Delete entities and cascade-delete their relations.
    pub async fn delete_entities(&self, entity_names: Vec<String>) -> Result<(), String> {
        let _guard = self.gate.lock().await;
        let mut graph = self.load().await?;
        graph.entities.retain(|e| !entity_names.contains(&e.name));
        graph
            .relations
            .retain(|r| !entity_names.contains(&r.from) && !entity_names.contains(&r.to));
        self.save(&graph).await
    }

    /// Delete specific observations from entities. Missing entities or
    /// observations are silently ignored.
    pub async fn delete_observations(
        &self,
        deletions: Vec<super::ObservationDeletion>,
    ) -> Result<(), String> {
        let _guard = self.gate.lock().await;
        let mut graph = self.load().await?;
        for deletion in deletions {
            if let Some(entity) = graph
                .entities
                .iter_mut()
                .find(|e| e.name == deletion.entity_name)
            {
                entity
                    .observations
                    .retain(|o| !deletion.observations.contains(o));
            }
        }
        self.save(&graph).await
    }

    /// Delete specific relations. Missing relations are silently ignored.
    pub async fn delete_relations(&self, relations: Vec<Relation>) -> Result<(), String> {
        let _guard = self.gate.lock().await;
        let mut graph = self.load().await?;
        graph.relations.retain(|r| {
            !relations.iter().any(|del| {
                r.from == del.from && r.to == del.to && r.relation_type == del.relation_type
            })
        });
        self.save(&graph).await
    }

    /// Read the entire knowledge graph.
    pub async fn read_graph(&self) -> Result<KnowledgeGraph, String> {
        let _guard = self.gate.lock().await;
        self.load().await
    }

    /// Search for entities whose name, type, or observations contain the
    /// query (case-insensitive). Relations with at least one matching
    /// endpoint are included, so callers can discover connections to nodes
    /// outside the result set.
    pub async fn search_nodes(&self, query: &str) -> Result<KnowledgeGraph, String> {
        let _guard = self.gate.lock().await;
        let graph = self.load().await?;
        let query = query.to_lowercase();

        let filtered_entities: Vec<Entity> = graph
            .entities
            .into_iter()
            .filter(|e| {
                e.name.to_lowercase().contains(&query)
                    || e.entity_type.to_lowercase().contains(&query)
                    || e.observations
                        .iter()
                        .any(|o| o.to_lowercase().contains(&query))
            })
            .collect();

        let filtered_names: Vec<&str> = filtered_entities.iter().map(|e| e.name.as_str()).collect();
        let filtered_relations: Vec<Relation> = graph
            .relations
            .into_iter()
            .filter(|r| {
                filtered_names.contains(&r.from.as_str()) || filtered_names.contains(&r.to.as_str())
            })
            .collect();

        Ok(KnowledgeGraph {
            entities: filtered_entities,
            relations: filtered_relations,
        })
    }

    /// Open specific entities by name. Relations with at least one endpoint
    /// in the requested set are included. Non-existent names are skipped.
    pub async fn open_nodes(&self, names: Vec<String>) -> Result<KnowledgeGraph, String> {
        let _guard = self.gate.lock().await;
        let graph = self.load().await?;

        let filtered_entities: Vec<Entity> = graph
            .entities
            .into_iter()
            .filter(|e| names.contains(&e.name))
            .collect();

        let filtered_names: Vec<&str> = filtered_entities.iter().map(|e| e.name.as_str()).collect();
        let filtered_relations: Vec<Relation> = graph
            .relations
            .into_iter()
            .filter(|r| {
                filtered_names.contains(&r.from.as_str()) || filtered_names.contains(&r.to.as_str())
            })
            .collect();

        Ok(KnowledgeGraph {
            entities: filtered_entities,
            relations: filtered_relations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(name: &str) -> Entity {
        Entity {
            name: name.to_string(),
            entity_type: "person".to_string(),
            observations: Vec::new(),
        }
    }

    #[tokio::test]
    async fn create_entities_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let manager = KnowledgeGraphManager::new(dir.path().join("memory.jsonl"));

        let added = manager
            .create_entities(vec![entity("alice"), entity("bob")])
            .await
            .unwrap();
        assert_eq!(added.len(), 2);

        // Same names again are ignored.
        let added = manager
            .create_entities(vec![entity("alice"), entity("carol")])
            .await
            .unwrap();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].name, "carol");

        let graph = manager.read_graph().await.unwrap();
        assert_eq!(graph.entities.len(), 3);
    }

    #[tokio::test]
    async fn relations_deduplicate() {
        let dir = tempfile::tempdir().unwrap();
        let manager = KnowledgeGraphManager::new(dir.path().join("memory.jsonl"));
        manager
            .create_entities(vec![entity("alice"), entity("bob")])
            .await
            .unwrap();

        let rel = || Relation {
            from: "alice".into(),
            to: "bob".into(),
            relation_type: "works_with".into(),
        };
        let added = manager.create_relations(vec![rel()]).await.unwrap();
        assert_eq!(added.len(), 1);
        let added = manager.create_relations(vec![rel()]).await.unwrap();
        assert_eq!(added.len(), 0, "exact duplicates are skipped");
    }

    #[tokio::test]
    async fn add_observations_fails_for_missing_entity() {
        let dir = tempfile::tempdir().unwrap();
        let manager = KnowledgeGraphManager::new(dir.path().join("memory.jsonl"));
        manager
            .create_entities(vec![entity("alice")])
            .await
            .unwrap();

        let err = manager
            .add_observations(vec![ObservationInput {
                entity_name: "nobody".into(),
                contents: vec!["x".into()],
            }])
            .await
            .unwrap_err();
        assert!(err.contains("Entity with name nobody not found"));

        let results = manager
            .add_observations(vec![ObservationInput {
                entity_name: "alice".into(),
                contents: vec![
                    "speaks Spanish".into(),
                    "speaks Spanish".into(),
                    "likes tea".into(),
                ],
            }])
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        // Like the reference server, duplicates within a single request are
        // all added; only pre-existing observations are filtered.
        assert_eq!(
            results[0].added_observations.len(),
            3,
            "duplicates within one call are added"
        );
    }

    #[tokio::test]
    async fn delete_entities_cascades_relations() {
        let dir = tempfile::tempdir().unwrap();
        let manager = KnowledgeGraphManager::new(dir.path().join("memory.jsonl"));
        manager
            .create_entities(vec![entity("alice"), entity("bob")])
            .await
            .unwrap();
        manager
            .create_relations(vec![Relation {
                from: "alice".into(),
                to: "bob".into(),
                relation_type: "knows".into(),
            }])
            .await
            .unwrap();

        manager.delete_entities(vec!["alice".into()]).await.unwrap();
        let graph = manager.read_graph().await.unwrap();
        assert_eq!(graph.entities.len(), 1);
        assert!(graph.relations.is_empty(), "relations cascade-deleted");

        // Silent when entity does not exist.
        manager.delete_entities(vec!["ghost".into()]).await.unwrap();
    }

    #[tokio::test]
    async fn search_and_open_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let manager = KnowledgeGraphManager::new(dir.path().join("memory.jsonl"));
        manager
            .create_entities(vec![
                Entity {
                    name: "alice".into(),
                    entity_type: "person".into(),
                    observations: vec!["speaks Spanish".into()],
                },
                Entity {
                    name: "acme".into(),
                    entity_type: "organization".into(),
                    observations: vec!["sells widgets".into()],
                },
                entity("bob"),
            ])
            .await
            .unwrap();
        manager
            .create_relations(vec![
                Relation {
                    from: "alice".into(),
                    to: "acme".into(),
                    relation_type: "works_at".into(),
                },
                Relation {
                    from: "alice".into(),
                    to: "bob".into(),
                    relation_type: "knows".into(),
                },
            ])
            .await
            .unwrap();

        // Search by name.
        let graph = manager.search_nodes("ALICE").await.unwrap();
        assert_eq!(graph.entities.len(), 1);
        // Relations to nodes outside the result set are included.
        assert_eq!(graph.relations.len(), 2);

        // Search by observation content.
        let graph = manager.search_nodes("spanish").await.unwrap();
        assert_eq!(graph.entities.len(), 1);
        assert_eq!(graph.entities[0].name, "alice");

        // Search with no matches.
        let graph = manager.search_nodes("nothing").await.unwrap();
        assert!(graph.entities.is_empty());
        assert!(graph.relations.is_empty());

        // Open nodes returns requested entities and their relations.
        let graph = manager
            .open_nodes(vec!["acme".into(), "ghost".into()])
            .await
            .unwrap();
        assert_eq!(graph.entities.len(), 1);
        assert_eq!(graph.entities[0].name, "acme");
        assert_eq!(graph.relations.len(), 1);
    }

    #[tokio::test]
    async fn persistence_across_manager_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.jsonl");
        {
            let manager = KnowledgeGraphManager::new(path.clone());
            manager
                .create_entities(vec![entity("alice")])
                .await
                .unwrap();
        }
        {
            let manager = KnowledgeGraphManager::new(path);
            let graph = manager.read_graph().await.unwrap();
            assert_eq!(graph.entities.len(), 1);
            assert_eq!(graph.entities[0].name, "alice");
        }
    }

    #[tokio::test]
    async fn missing_file_loads_empty_graph() {
        let dir = tempfile::tempdir().unwrap();
        let manager = KnowledgeGraphManager::new(dir.path().join("does-not-exist.jsonl"));
        let graph = manager.read_graph().await.unwrap();
        assert!(graph.entities.is_empty());
        assert!(graph.relations.is_empty());
    }
}
