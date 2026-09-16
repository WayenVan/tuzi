#[allow(dead_code)]
pub trait Actor {
	type Form;

	fn act(form: Self::Form);
}
