# Pool borné pour les arrivées simultanées

Date : 2026-10-03. Statut : prototype mesuré ; qualification production en cours.

## Problème et décision

Les snapshots de v0.52.6 partagent la RAM et évitent le boot ordinaire. Une seule
VM anonyme est pourtant prête : trois conversations d'une rafale de quatre
copient encore leur journal et renouvellent Codex dans leur chemin critique.
Les restaurations se chevauchent déjà ; les sérialiser davantage ne résout rien.

Le pool accepte une cible explicite de une à quatre VM, par défaut une.
`LEO_READY_VM_POOL_SIZE` est conservé par le déploiement Compose ; le superviseur
utilise `readyVmPoolSize`. La préparation reste séquentielle en arrière-plan.
Chaque VM est anonyme, sans accès, attribuée une seule fois et réutilise les
garanties de renouvellement d'identité du snapshot.

Les VM préparées occupent les vrais slots et leur RAM compte dans le cgroup.
Pour ajouter un spare, leur coût résident mesuré plus 2 Gio de marge doit tenir
dans un quart du budget mémoire du nœud, borné entre 2 et 8 Gio. Le pool réévalue
aussi son coût après préparation et retire les plus anciennes VM excédentaires.
Le premier spare garde la politique existante sur les petits nœuds. Les seuils
de pression et le plafond mémoire partagé restent applicables à tout le pool.

Le travail actif évince d'abord les conversations retenues les plus anciennes
dont la publication est acquittée, puis les spares les plus anciens. Il n'évince
jamais une conversation non publiée pour agrandir le pool. Une préparation
annulée libère son slot uniquement après arrêt de son VMM et de son backend.
Le pool ne promet pas quatre hits quand les ressources ou les slots manquent.

## Diagnostic de production avant changement

Quatre nouvelles conversations, même agent/compte/modèle, après déploiement
v0.52.6. Le délai se termine au premier appel effectif du modèle, après le
préchauffage `generate=false`, pas à la première activité affichée.

| Phase | Hors pool, trois conversations | Hit du pool |
| --- | ---: | ---: |
| Admission manager | 1,32–4,67 s | 0,17 s |
| Préparation disque et VM sur le nœud | 3,30–3,56 s | 0,025 s |
| Copie du journal, incluse dans la ligne précédente | 1,22–1,42 s | aucune |
| Identité/Codex et montage workspace, inclus dans la préparation | 1,22–1,41 s | déjà fait |
| Requête guest vers premier appel modèle | 3,90–4,53 s | 2,98 s |
| Préchauffage WebSocket, inclus dans la ligne précédente | 1,69–1,83 s | 1,55 s |
| Délai total avant modèle | 10,54–13,29 s | 3,65 s |

Ces phases incluent des sous-phases : ne pas additionner toutes les lignes.
La copie est corrélée au début de boot par leurs timestamps adjacents ; les
identifiants physiques des environnements relient les trois traces de boot aux
conversations. Ce relevé n'avait pas de capture CPU : il ne prouve ni une
saturation CPU ni une cause réseau pour le temps restant dans Codex.

Une seconde rafale, avec échantillonnage commencé avant les envois, confirme
4,26 s pour le hit et 11,27–12,78 s pour les trois autres. Sur la fenêtre de
13,03 s qui couvre les quatre premiers appels, le cgroup utilise en moyenne
2,28 cœurs sur sept ; aucun des 131 intervalles CPU n'est bridé. Le compteur
PSI `io.some` augmente de 9,34 s, celui de l'hôte de 8,78 s : au moins une tâche
attend une I/O pendant une grande partie de la fenêtre. Ce n'est pas un délai
attribuable à chaque conversation ni une preuve causale isolée. L'admission
des trois misses reste espacée à 1,35 ; 2,86 et 4,36 s après leurs envois.

## Premier comparatif natif

Même image prototype, Firecracker 1.17, ublk Async, vrai Codex, enveloppe de
7 CPU/36 Gio et huit slots. Compte et modèle synthétiques ; stockage HTTP local.
Quatre requêtes simultanées, pool rempli avant chaque essai.

| Cible du pool | Délais avant modèle des quatre arrivées |
| --- | --- |
| 1 | 3,404 ; 3,691 ; 3,585 ; 3,409 s |
| 4 | 1,891 ; 1,895 ; 2,009 ; 1,893 s |

La médiane baisse de 45,8 %. Les huit conversations exécutent leur outil,
publient leurs blocs vérifiés et reprennent le même thread après SIGKILL du
contrôleur. Ce banc ne mesure ni l'admission du manager ni S3/WAN. Il ne valide
donc pas encore l'objectif de moins de trois secondes en production.

Un deuxième essai avec les timings supplémentaires donne 2,173–2,175 s pour
les quatre arrivées. Il vérifie que leurs identifiants sont ceux des quatre VM
déjà préparées. Le pool plein, template et caches inclus, occupe 2,93 Gio dans
le cgroup avant les envois. Les quatre publications et reprises après crash
passent également, avec 194 blocs vérifiés sur les quatre disques.

Les mesures distinguent les quatre VMM des conversations des spares reconstitués
pendant leur exécution. Le cgroup inclut tous les VMM, les backends et le cache.
Les traces supplémentaires séparent l'attente du verrou de préparation manager,
la préemption du remplissage du pool, le renouvellement réseau/entropie et le
redémarrage natif. La trace du clone porte désormais l'identifiant de son
environnement, pour éviter une attribution ambiguë au template commun.

Les résultats bruts et scripts de qualification restent hors de Git. Ils seront
joints à la release seulement après confirmation d'un gain en production.
