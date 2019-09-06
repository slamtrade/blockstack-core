use vm::errors::{Error as InterpError, RuntimeErrorType};
use vm::functions::NativeFunctions;
use vm::{ClarityName, SymbolicExpression};
use vm::types::{BUFF_32, BUFF_20, TypeSignature, TupleTypeSignature, BlockInfoProperty,
                MAX_VALUE_SIZE, FunctionArg, FunctionType, FixedFunction};
use super::{TypeChecker, TypingContext, TypeResult, no_type, check_argument_count, check_arguments_at_least}; 
use vm::analysis::errors::{CheckError, CheckErrors, CheckResult};
use std::convert::TryFrom;

mod assets;
mod lists;
mod maps;
mod options;

pub enum TypedNativeFunction {
    Special(SpecialNativeFunction),
    Simple(SimpleNativeFunction)
}

pub struct SpecialNativeFunction(&'static Fn(&mut TypeChecker, &[SymbolicExpression], &TypingContext) -> TypeResult);
pub struct SimpleNativeFunction(pub FunctionType);

fn check_special_list_cons(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    let typed_args = checker.type_check_all(args, context)?;
    TypeSignature::parent_list_type(&typed_args)
        .map_err(|x| x.into())
        .map(TypeSignature::from)
}

fn check_special_print(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_argument_count(1, args)?;
    checker.type_check(&args[0], context)
}

fn check_special_as_contract(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_argument_count(1, args)?;
    checker.type_check(&args[0], context)
}

fn check_special_begin(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_arguments_at_least(1, args)?;
        
    let mut typed_args = checker.type_check_all(args, context)?;
    
    let last_return = typed_args.pop()
        .ok_or(CheckError::new(CheckErrors::CheckerImplementationFailure))?;
    
    Ok(last_return)
}

fn inner_handle_tuple_get(tuple_type_sig: &TupleTypeSignature, field_to_get: &str) -> TypeResult {
    let return_type = tuple_type_sig.field_type(field_to_get)
        .ok_or(CheckError::new(CheckErrors::NoSuchTupleField(field_to_get.to_string(), tuple_type_sig.clone())))?
        .clone();
    Ok(return_type)
}

fn check_special_get(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_argument_count(2, args)?;
    
    let field_to_get = args[0].match_atom()
        .ok_or(CheckErrors::BadTupleFieldName)?;
    
    checker.type_map.set_type(&args[0], no_type())?;
    
    let argument_type = checker.type_check(&args[1], context)?;
    
    if let TypeSignature::TupleType(tuple_type_sig) = argument_type {
        inner_handle_tuple_get(&tuple_type_sig, field_to_get)
    } else if let TypeSignature::OptionalType(value_type_sig) = argument_type {
        if let TypeSignature::TupleType(tuple_type_sig) = *value_type_sig {
            let inner_type = inner_handle_tuple_get(&tuple_type_sig, field_to_get)?;
            let option_type = TypeSignature::new_option(inner_type);
            Ok(option_type)
        } else {
            Err(CheckErrors::ExpectedTuple(*value_type_sig).into())
        }
    } else {
        Err(CheckErrors::ExpectedTuple(argument_type).into())
    }
}

pub fn check_special_tuple_cons(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_arguments_at_least(1, args)?;
    
    let mut tuple_type_data = Vec::new();
    for pair in args.iter() {
        let pair_expression = pair.match_list()
            .ok_or(CheckErrors::TupleExpectsPairs)?;
        if pair_expression.len() != 2 {
            return Err(CheckError::new(CheckErrors::TupleExpectsPairs))
        }
        
        let var_name = pair_expression[0].match_atom()
            .ok_or(CheckErrors::TupleExpectsPairs)?;
        checker.type_map.set_type(&pair_expression[0], no_type())?;
        
        let var_type = checker.type_check(&pair_expression[1], context)?;
        tuple_type_data.push((var_name.clone(), var_type))
    }
    
    let tuple_signature = TupleTypeSignature::try_from(tuple_type_data)
        .map_err(|_| CheckErrors::BadTupleConstruction)?;
    
    Ok(TypeSignature::TupleType(tuple_signature))
}

fn check_special_let(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_arguments_at_least(2, args)?;

    checker.type_map.set_type(&args[0], no_type())?;

    let binding_list = args[0].match_list()
        .ok_or(CheckError::new(CheckErrors::BadLetSyntax))?;
    
    let mut out_context = context.extend()?;

    for binding in binding_list.iter() {
        let binding_exps = binding.match_list()
            .ok_or(CheckError::new(CheckErrors::BadSyntaxBinding))?;
        
        if binding_exps.len() != 2 {
            return Err(CheckError::new(CheckErrors::BadSyntaxBinding))
        }

        let var_name = binding_exps[0].match_atom()
            .ok_or(CheckError::new(CheckErrors::BadSyntaxBinding))?;

        checker.contract_context.check_name_used(var_name)?;

        if out_context.lookup_variable_type(var_name).is_some() {
            return Err(CheckError::new(CheckErrors::NameAlreadyUsed(var_name.to_string())))
        }

        checker.type_map.set_type(&binding_exps[0], no_type())?;
        let typed_result = checker.type_check(&binding_exps[1], context)?;
        out_context.variable_types.insert(var_name.clone(), typed_result);
    }
    
    let mut typed_args = checker.type_check_all(&args[1..args.len()], &out_context)?;
    
    let last_return = typed_args.pop()
        .ok_or(CheckError::new(CheckErrors::CheckerImplementationFailure))?;
    
    Ok(last_return)
}

fn check_special_fetch_var(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_argument_count(1, args)?;
    
    let var_name = args[0].match_atom()
        .ok_or(CheckError::new(CheckErrors::BadMapName))?;
    
    checker.type_map.set_type(&args[0], no_type())?;
        
    let value_type = checker.contract_context.get_persisted_variable_type(var_name)
        .ok_or(CheckError::new(CheckErrors::NoSuchDataVariable(var_name.to_string())))?;

    Ok(value_type.clone())
}

fn check_special_set_var(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_arguments_at_least(2, args)?;
    
    let var_name = args[0].match_atom()
        .ok_or(CheckErrors::BadMapName)?;
    
    checker.type_map.set_type(&args[0], no_type())?;
    
    let value_type = checker.type_check(&args[1], context)?;
    
    let expected_value_type = checker.contract_context.get_persisted_variable_type(var_name)
        .ok_or(CheckErrors::NoSuchDataVariable(var_name.to_string()))?;
    
    if !expected_value_type.admits_type(&value_type) {
        return Err(CheckError::new(CheckErrors::TypeError(expected_value_type.clone(), value_type)))
    } else {
        return Ok(TypeSignature::BoolType)
    }
}

fn check_special_equals(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_arguments_at_least(1, args)?;

    let mut arg_types = checker.type_check_all(args, context)?;

    let mut arg_type = arg_types[0].clone();
    for x_type in arg_types.drain(..) {
        arg_type = TypeSignature::least_supertype(&x_type, &arg_type)
            .map_err(|_| CheckErrors::TypeError(x_type, arg_type))?;

    }

    Ok(TypeSignature::BoolType)
}

fn check_special_if(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_argument_count(3, args)?;
    
    checker.type_check_expects(&args[0], context, &TypeSignature::BoolType)?;

    let arg_types = checker.type_check_all(&args[1..], context)?;
    
    let expr1 = &arg_types[0];
    let expr2 = &arg_types[1];

    TypeSignature::least_supertype(expr1, expr2)
        .map_err(|_| CheckErrors::IfArmsMustMatch(expr1.clone(), expr2.clone()).into())
}

fn check_contract_call(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_arguments_at_least(2, args)?;
    let contract_name = args[0].match_atom()
        .ok_or(CheckError::new(CheckErrors::ContractCallExpectName))?;
    let function_name = args[1].match_atom()
        .ok_or(CheckError::new(CheckErrors::ContractCallExpectName))?;
    checker.type_map.set_type(&args[0], no_type())?;
    checker.type_map.set_type(&args[1], no_type())?;

    let contract_call_function_type = {
        if let Some(function_type) = checker.db.get_public_function_type(contract_name, function_name)? {
            Ok(function_type)
        } else if let Some(function_type) = checker.db.get_read_only_function_type(contract_name, function_name)? {
            Ok(function_type)
        } else {
            Err(CheckError::new(CheckErrors::NoSuchPublicFunction(contract_name.to_string(),
                                                                  function_name.to_string())))
        }
    }?;

    let contract_call_args = checker.type_check_all(&args[2..], context)?;
    
    let return_type = contract_call_function_type.check_args(&contract_call_args)?;
    
    Ok(return_type)
}

fn check_get_block_info(checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
    check_arguments_at_least(2, args)?;

    checker.type_map.set_type(&args[0], no_type())?;
    let block_info_prop_str = args[0].match_atom()
        .ok_or(CheckError::new(CheckErrors::GetBlockInfoExpectPropertyName))?;

    let block_info_prop = BlockInfoProperty::lookup_by_name(block_info_prop_str)
        .ok_or(CheckError::new(CheckErrors::NoSuchBlockInfoProperty(block_info_prop_str.to_string())))?;

    checker.type_check_expects(&args[1], &context, &TypeSignature::IntType)?;
        
    Ok(block_info_prop.type_result())
}

impl TypedNativeFunction {
    pub fn type_check_appliction(&self, checker: &mut TypeChecker, args: &[SymbolicExpression], context: &TypingContext) -> TypeResult {
        use self::TypedNativeFunction::{Special, Simple};
        match self {
            Special(SpecialNativeFunction(check)) => check(checker, args, context),
            Simple(SimpleNativeFunction(function_type)) => checker.type_check_function_type(function_type, args, context),
        }
    }

    pub fn type_native_function(function: &NativeFunctions) -> TypedNativeFunction {
        use self::TypedNativeFunction::{Special, Simple};
        use vm::functions::NativeFunctions::*;
        match function {
            Add | Subtract | Divide | Multiply =>
                Simple(SimpleNativeFunction(FunctionType::ArithmeticVariadic)),
            CmpGeq | CmpLeq | CmpLess | CmpGreater =>
                Simple(SimpleNativeFunction(FunctionType::ArithmeticComparison)),
            Modulo | Power | BitwiseXOR =>
                Simple(SimpleNativeFunction(FunctionType::ArithmeticBinary)),
            And | Or =>
                Simple(SimpleNativeFunction(FunctionType::Variadic(TypeSignature::BoolType,
                                                                   TypeSignature::BoolType))),
            ToUInt =>
                Simple(SimpleNativeFunction(FunctionType::Fixed(FixedFunction {
                    args: vec![FunctionArg::new(TypeSignature::IntType,
                                                ClarityName::try_from("value".to_owned())
                                                .expect("FAIL: ClarityName failed to accept default arg name"))],
                    returns: TypeSignature::UIntType }))),
            ToInt =>
                Simple(SimpleNativeFunction(FunctionType::Fixed(FixedFunction {
                    args: vec![FunctionArg::new(TypeSignature::UIntType,
                                                ClarityName::try_from("value".to_owned())
                                                .expect("FAIL: ClarityName failed to accept default arg name"))],
                    returns: TypeSignature::IntType }))),
            Not =>
                Simple(SimpleNativeFunction(FunctionType::Fixed(FixedFunction { 
                    args: vec![FunctionArg::new(TypeSignature::BoolType, ClarityName::try_from("value".to_owned())
                                                .expect("FAIL: ClarityName failed to accept default arg name"))],
                    returns: TypeSignature::BoolType }))),
            Hash160 =>
                Simple(SimpleNativeFunction(FunctionType::UnionArgs(
                    vec![TypeSignature::max_buffer(),
                         TypeSignature::IntType],
                    BUFF_20.clone()))),
            Sha256 =>
                Simple(SimpleNativeFunction(FunctionType::UnionArgs(
                    vec![TypeSignature::max_buffer(),
                         TypeSignature::IntType],
                    BUFF_32.clone()))),
            Keccak256 =>
                Simple(SimpleNativeFunction(FunctionType::UnionArgs(
                    vec![TypeSignature::max_buffer(),
                         TypeSignature::IntType],
                    BUFF_32.clone()))),
            GetTokenBalance => Special(SpecialNativeFunction(&assets::check_special_get_balance)),
            GetAssetOwner => Special(SpecialNativeFunction(&assets::check_special_get_owner)),
            TransferToken => Special(SpecialNativeFunction(&assets::check_special_transfer_token)),
            TransferAsset => Special(SpecialNativeFunction(&assets::check_special_transfer_asset)),
            MintAsset => Special(SpecialNativeFunction(&assets::check_special_mint_asset)),
            MintToken => Special(SpecialNativeFunction(&assets::check_special_mint_token)),
            Equals => Special(SpecialNativeFunction(&check_special_equals)),
            If => Special(SpecialNativeFunction(&check_special_if)),
            Let => Special(SpecialNativeFunction(&check_special_let)),
            FetchVar => Special(SpecialNativeFunction(&check_special_fetch_var)),
            SetVar => Special(SpecialNativeFunction(&check_special_set_var)),
            Map => Special(SpecialNativeFunction(&lists::check_special_map)),
            Filter => Special(SpecialNativeFunction(&lists::check_special_filter)),
            Fold => Special(SpecialNativeFunction(&lists::check_special_fold)),
            ListCons => Special(SpecialNativeFunction(&check_special_list_cons)),
            FetchEntry => Special(SpecialNativeFunction(&maps::check_special_fetch_entry)),
            FetchContractEntry => Special(SpecialNativeFunction(&maps::check_special_fetch_contract_entry)),
            SetEntry => Special(SpecialNativeFunction(&maps::check_special_set_entry)),
            InsertEntry => Special(SpecialNativeFunction(&maps::check_special_insert_entry)),
            DeleteEntry => Special(SpecialNativeFunction(&maps::check_special_delete_entry)),
            TupleCons => Special(SpecialNativeFunction(&check_special_tuple_cons)),
            TupleGet => Special(SpecialNativeFunction(&check_special_get)),
            Begin => Special(SpecialNativeFunction(&check_special_begin)),
            Print => Special(SpecialNativeFunction(&check_special_print)),
            AsContract => Special(SpecialNativeFunction(&check_special_as_contract)),
            ContractCall => Special(SpecialNativeFunction(&check_contract_call)),
            GetBlockInfo => Special(SpecialNativeFunction(&check_get_block_info)),
            ConsSome => Special(SpecialNativeFunction(&options::check_special_some)),
            ConsOkay => Special(SpecialNativeFunction(&options::check_special_okay)),
            ConsError => Special(SpecialNativeFunction(&options::check_special_error)),
            DefaultTo => Special(SpecialNativeFunction(&options::check_special_default_to)),
            Expects => Special(SpecialNativeFunction(&options::check_special_expects)),
            ExpectsErr => Special(SpecialNativeFunction(&options::check_special_expects_err)),
            IsOkay => Special(SpecialNativeFunction(&options::check_special_is_okay)),
            IsNone => Special(SpecialNativeFunction(&options::check_special_is_none))
        }
    }
}