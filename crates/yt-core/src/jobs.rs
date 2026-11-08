use crate::{AppCore, CoreError, Job, JobStatus, Result, now};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    Ok(Job {
        id: row.get("id")?,
        kind: row.get("kind")?,
        status: JobStatus::parse(&row.get::<_, String>("status")?),
        progress: row.get("progress")?,
        message: row.get("message")?,
        payload: serde_json::from_str(&row.get::<_, String>("payload_json")?)
            .unwrap_or(Value::Null),
        result: serde_json::from_str(&row.get::<_, String>("result_json")?).unwrap_or(Value::Null),
        error: row.get("error")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl AppCore {
    pub fn create_job(&self, kind: &str, payload: &Value) -> Result<Job> {
        if kind.trim().is_empty() || kind.len() > 80 {
            return Err(CoreError::Invalid("Invalid job kind".into()));
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.connection()?.execute("INSERT INTO jobs(id,kind,status,payload_json,created_at,updated_at) VALUES(?1,?2,'pending',?3,?4,?4)",
            params![id,kind,serde_json::to_string(payload)?,now()])?;
        self.job(&id)
    }

    pub fn job(&self, id: &str) -> Result<Job> {
        self.connection()?
            .query_row("SELECT * FROM jobs WHERE id=?1", [id], job_from_row)
            .optional()?
            .ok_or_else(|| CoreError::NotFound(id.into()))
    }

    pub fn jobs(&self) -> Result<Vec<Job>> {
        let db = self.connection()?;
        let mut query =
            db.prepare("SELECT * FROM jobs ORDER BY created_at DESC,id DESC LIMIT 500")?;
        Ok(query
            .query_map([], job_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// All downloads referring to a resumable cache, including older retries
    /// outside the bounded history shown in the interface.
    pub fn jobs_for_download_cache(&self, cache_id: &str) -> Result<Vec<Job>> {
        if cache_id.is_empty() {
            return Ok(Vec::new());
        }
        let db = self.connection()?;
        let mut query = db.prepare(
            "SELECT * FROM jobs WHERE kind='download' AND (
                CASE WHEN json_valid(payload_json) THEN
                    json_extract(payload_json,'$.cache_id')=?1 OR
                    (id=?1 AND COALESCE(json_extract(payload_json,'$.cache_id'),'')='')
                ELSE id=?1 END
            ) ORDER BY created_at DESC,id DESC",
        )?;
        Ok(query
            .query_map([cache_id], job_from_row)?
            .collect::<std::result::Result<_, _>>()?)
    }

    pub fn update_job(
        &self,
        id: &str,
        status: JobStatus,
        progress: f64,
        message: &str,
        result: &Value,
        error: &str,
    ) -> Result<Job> {
        if !progress.is_finite() || !(0.0..=100.0).contains(&progress) {
            return Err(CoreError::Invalid(
                "Job progress must be between 0 and 100".into(),
            ));
        }
        {
            let mut db = self.connection()?;
            let tx = db.transaction()?;
            let existing = tx
                .query_row("SELECT * FROM jobs WHERE id=?1", [id], job_from_row)
                .optional()?
                .ok_or_else(|| CoreError::NotFound(id.into()))?;
            if existing.status.is_terminal() && status != existing.status {
                return Err(CoreError::Invalid(
                    "A finished job cannot transition to another state; create a new job to retry"
                        .into(),
                ));
            }
            if existing.status == JobStatus::Running && status == JobStatus::Pending {
                return Err(CoreError::Invalid(
                    "A running job cannot become pending".into(),
                ));
            }
            let timestamp = now();
            tx.execute("UPDATE jobs SET status=?1,progress=?2,message=?3,result_json=?4,error=?5,updated_at=?6,
                started_at=CASE WHEN ?1='running' THEN COALESCE(started_at,?6) ELSE started_at END,
                finished_at=CASE WHEN ?7 THEN COALESCE(finished_at,?6) ELSE finished_at END WHERE id=?8",
                params![status.as_str(),if status==JobStatus::Completed{100.0}else{progress},message,serde_json::to_string(result)?,error,timestamp,status.is_terminal(),id])?;
            tx.commit()?;
        }
        self.job(id)
    }

    pub fn cancel_job(&self, id: &str) -> Result<Job> {
        let job = self.job(id)?;
        self.update_job(
            id,
            JobStatus::Canceled,
            job.progress,
            "Cancelado",
            &job.result,
            "",
        )
    }

    pub fn remove_job(&self, id: &str) -> Result<()> {
        let db = self.connection()?;
        let changed=db.execute("DELETE FROM jobs WHERE id=?1 AND status NOT IN ('pending','queued','running','processing','cancelling')",[id])?;
        if changed == 0 {
            return Err(CoreError::Invalid(
                "Only finished jobs can be removed".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn recover_interrupted_jobs(&self) -> Result<usize> {
        Ok(self.connection()?.execute("UPDATE jobs SET status='interrupted',message='Interrumpido al cerrar la aplicación; puedes volver a intentarlo',
            error='Application stopped before the job finished',finished_at=?1,updated_at=?1
            WHERE status IN ('pending','queued','running','processing','cancelling')",[now()])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_cache_lookup_keeps_older_retries_beyond_display_limit() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let original = core.create_job("download", &serde_json::json!({})).unwrap();
        let retry = core
            .create_job("download", &serde_json::json!({"cache_id": original.id}))
            .unwrap();
        core.create_job("stems", &serde_json::json!({"cache_id": original.id}))
            .unwrap();
        core.create_job("download", &serde_json::json!({"cache_id": "different"}))
            .unwrap();
        for _ in 0..501 {
            core.create_job("download", &serde_json::json!({})).unwrap();
        }
        assert_eq!(core.jobs().unwrap().len(), 500);
        assert!(!core.jobs().unwrap().iter().any(|job| job.id == original.id));
        let jobs = core.jobs_for_download_cache(&original.id).unwrap();
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().any(|job| job.id == original.id));
        assert!(jobs.iter().any(|job| job.id == retry.id));
        core.connection()
            .unwrap()
            .execute(
                "UPDATE jobs SET payload_json='invalid legacy JSON' WHERE id=?1",
                [&original.id],
            )
            .unwrap();
        assert_eq!(core.jobs_for_download_cache(&original.id).unwrap().len(), 2);
        assert!(core.jobs_for_download_cache("").unwrap().is_empty());
    }
}
