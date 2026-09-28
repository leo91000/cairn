# Publication du disque courant sans archives

Le propriétaire ne souhaite ni archivage des conversations ni historique de restauration. Le journal local durable reste synchronisé de façon asynchrone vers S3 ; une publication remplace l’état courant après vérification de tous ses blocs. Les versions antérieures ne subsistent que tant qu’un disque ou une opération en cours les référence, puis sont collectées, y compris pour les conversations inactives.

Les anciennes archives doivent être supprimées avant déploiement : leur création, leur restauration et les transitions froides sont retirées. La corbeille reste un mécanisme de suppression avec récupération pendant 30 jours. Cette décision remplace les aspects archivage et rétention des ADR-0001, ADR-0004 et ADR-0006 ; elle ne transforme pas les écritures locales en écritures synchrones S3 et ne couvre pas la sauvegarde de la base applicative ni de la clé de chiffrement.
